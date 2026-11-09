#include "CrossBenchSubsystem.h"

#include "Engine/World.h"
#include "HAL/PlatformMisc.h"
#include "HAL/PlatformTime.h"
#include "Misc/CommandLine.h"
#include "Misc/EngineVersion.h"
#include "Misc/Parse.h"
#include "Stats/Stats.h"

DEFINE_LOG_CATEGORY(LogCrossBench);

namespace
{
    // The fixture constants. These match examples/cross_bench.rs and benchmarks/godot/cross_bench.gd, because a
    // comparison that changes the workload between engines is not a comparison.
    constexpr int32 DefaultAgents = 10000;
    constexpr int32 DefaultTicks = 1000;
    constexpr int32 DefaultWarmup = 100;
    constexpr int32 DefaultSeed = 0xC0FFEE;
    constexpr double DefaultBudgetMs = 16.6;
    constexpr double HalfExtent = 500.0;
    constexpr double Speed = 4.0;
    constexpr double CellSize = 10.0;
    constexpr double SeekLerp = 0.05;
    constexpr double SeparationPush = 0.05;
    constexpr double TwoPi = 6.283185307179586;
    constexpr double TinySquared = 1.0e-8;
    constexpr double FixedStepSeconds = 1.0 / 60.0;
    constexpr int32 MaxNeighboursRead = 8;

    int32 ReadInt(const TCHAR* Key, int32 Fallback)
    {
        int32 Value = Fallback;
        FParse::Value(FCommandLine::Get(), Key, Value);
        return Value;
    }

    double ReadDouble(const TCHAR* Key, double Fallback)
    {
        double Value = Fallback;
        FParse::Value(FCommandLine::Get(), Key, Value);
        return Value;
    }

    FString ReadString(const TCHAR* Key)
    {
        FString Value;
        return FParse::Value(FCommandLine::Get(), Key, Value) ? Value : FString();
    }

    /** Nearest-rank percentile over an ascending array, the same definition the other harnesses use. */
    double Percentile(const TArray<double>& Ascending, double Fraction)
    {
        if (Ascending.IsEmpty())
        {
            return 0.0;
        }
        const int32 Rank = FMath::Clamp(FMath::CeilToInt(Fraction * Ascending.Num()), 1, Ascending.Num());
        return Ascending[Rank - 1];
    }

    /** The machine string, built the same way the Rust harness builds it so the comparator sees one machine. */
    FString MachineDescription(const FString& Override)
    {
        if (!Override.IsEmpty())
        {
            return Override;
        }
        FString Brand = FPlatformMisc::GetCPUBrand();
        if (Brand.IsEmpty())
        {
            Brand = TEXT("unknown cpu");
        }
        return FString::Printf(TEXT("%s, %d threads"), *Brand, FPlatformMisc::NumberOfCoresIncludingHyperthreads());
    }
}

void UCrossBenchSubsystem::Initialize(FSubsystemCollectionBase& Collection)
{
    Super::Initialize(Collection);

    Agents = FMath::Max(1, ReadInt(TEXT("-crossbenchagents="), DefaultAgents));
    Ticks = FMath::Max(1, ReadInt(TEXT("-crossbenchticks="), DefaultTicks));
    Warmup = FMath::Max(0, ReadInt(TEXT("-crossbenchwarmup="), DefaultWarmup));
    Seed = ReadInt(TEXT("-crossbenchseed="), DefaultSeed);
    BudgetMs = ReadDouble(TEXT("-crossbenchbudget="), DefaultBudgetMs);
    MachineOverride = ReadString(TEXT("-crossbenchmachine="));

    // The grid covers the cube exactly, so no body can fall outside every cell because of a rounding choice. Each
    // axis gets its own divisor so the last cell is never a sliver.
    const int32 Axis = FMath::Max(1, FMath::RoundToInt((HalfExtent * 2.0) / CellSize));
    GridDims = FIntVector(Axis, Axis, Axis);
    CellSize = (HalfExtent * 2.0) / static_cast<double>(Axis);

    Positions.SetNumUninitialized(Agents);
    Velocities.SetNumUninitialized(Agents);

    // The same distribution the other harnesses draw: uniform over the cube, heading uniform in the plane, one speed.
    FRandomStream Stream(Seed);
    for (int32 Index = 0; Index < Agents; ++Index)
    {
        Positions[Index] = FVector(
            -HalfExtent + 2.0 * HalfExtent * Stream.GetFraction(),
            -HalfExtent + 2.0 * HalfExtent * Stream.GetFraction(),
            -HalfExtent + 2.0 * HalfExtent * Stream.GetFraction());
        const double Angle = TwoPi * Stream.GetFraction();
        Velocities[Index] = FVector(FMath::Cos(Angle), FMath::Sin(Angle), 0.0) * Speed;
    }

    Samples.Reserve(Ticks);
    RebuildSamples.Reserve(Ticks);
    StepSamples.Reserve(Ticks);

    bRunning = true;

    UE_LOG(
        LogCrossBench,
        Display,
        TEXT("crossbench starting: %d agents, %d measured ticks, %d warm-up, grid %dx%dx%d at %.2f m, fixed step "
             "%.4f s"),
        Agents,
        Ticks,
        Warmup,
        GridDims.X,
        GridDims.Y,
        GridDims.Z,
        CellSize,
        FixedStepSeconds);
}

bool UCrossBenchSubsystem::DoesSupportWorldType(const EWorldType::Type WorldType) const
{
    // Game and PIE only: an editor world would run the fixture on every map open.
    return WorldType == EWorldType::Game || WorldType == EWorldType::PIE;
}

TStatId UCrossBenchSubsystem::GetStatId() const
{
    RETURN_QUICK_DECLARE_CYCLE_STAT(UCrossBenchSubsystem, STATGROUP_Tickables);
}

void UCrossBenchSubsystem::Tick(float DeltaTime)
{
    Super::Tick(DeltaTime);

    if (!bRunning)
    {
        return;
    }

    if (TickIndex >= Warmup + Ticks)
    {
        bRunning = false;
        Report();
        return;
    }

    const double Started = FPlatformTime::Seconds();
    RebuildGrid();
    const double Rebuilt = FPlatformTime::Seconds();
    Step();
    const double Finished = FPlatformTime::Seconds();

    if (TickIndex >= Warmup)
    {
        // Milliseconds, matching the other two harnesses, kept in three arrays so the report can attribute the tick
        // instead of offering one number with a story attached.
        Samples.Add((Finished - Started) * 1000.0);
        RebuildSamples.Add((Rebuilt - Started) * 1000.0);
        StepSamples.Add((Finished - Rebuilt) * 1000.0);
    }

    ++TickIndex;
}

int32 UCrossBenchSubsystem::CellIndex(const FVector& Position) const
{
    const int32 X = FMath::Clamp(FMath::FloorToInt((Position.X + HalfExtent) / CellSize), 0, GridDims.X - 1);
    const int32 Y = FMath::Clamp(FMath::FloorToInt((Position.Y + HalfExtent) / CellSize), 0, GridDims.Y - 1);
    const int32 Z = FMath::Clamp(FMath::FloorToInt((Position.Z + HalfExtent) / CellSize), 0, GridDims.Z - 1);
    return (X * GridDims.Y + Y) * GridDims.Z + Z;
}

void UCrossBenchSubsystem::RebuildGrid()
{
    const int32 Cells = GridDims.X * GridDims.Y * GridDims.Z;

    // Counts land in slot cell + 1, so the prefix scan leaves slot cell holding that cell's start and the last slot
    // holding the total. Identical arithmetic to crates/bevy-flock-core's SpatialGrid, on purpose.
    CellStart.Reset();
    CellStart.SetNumZeroed(Cells + 1);
    for (int32 Index = 0; Index < Agents; ++Index)
    {
        ++CellStart[CellIndex(Positions[Index]) + 1];
    }
    for (int32 Cell = 1; Cell <= Cells; ++Cell)
    {
        CellStart[Cell] += CellStart[Cell - 1];
    }

    CellCursor = CellStart;
    Sorted.SetNumUninitialized(Agents);
    for (int32 Index = 0; Index < Agents; ++Index)
    {
        Sorted[CellCursor[CellIndex(Positions[Index])]++] = Index;
    }
}

void UCrossBenchSubsystem::Step()
{
    for (int32 Index = 0; Index < Agents; ++Index)
    {
        const FVector Position = Positions[Index];
        const FVector Desired = (FVector::ZeroVector - Position).GetSafeNormal() * Speed;
        FVector Velocity = FMath::Lerp(Velocities[Index], Desired, SeekLerp);

        // Local separation, bounded: at most MaxNeighboursRead agents from the same cell. Unbounded neighbour reads
        // would make the tick O(N * occupancy) and turn a crowd number into a density measurement.
        const int32 Cell = CellIndex(Position);
        const int32 Begin = CellStart[Cell];
        const int32 End = FMath::Min(CellStart[Cell + 1], Begin + MaxNeighboursRead);
        for (int32 Slot = Begin; Slot < End; ++Slot)
        {
            const int32 Other = Sorted[Slot];
            if (Other == Index)
            {
                continue;
            }
            const FVector Away = Position - Positions[Other];
            const double DistanceSquared = Away.SizeSquared();
            if (DistanceSquared > TinySquared && DistanceSquared < CellSize * CellSize)
            {
                Velocity += Away.GetSafeNormal() * SeparationPush;
            }
        }

        Velocities[Index] = Velocity;
        const FVector Moved = Position + Velocity * FixedStepSeconds;
        Positions[Index] = FVector(
            FMath::Clamp(Moved.X, -HalfExtent, HalfExtent),
            FMath::Clamp(Moved.Y, -HalfExtent, HalfExtent),
            FMath::Clamp(Moved.Z, -HalfExtent, HalfExtent));
    }
}

void UCrossBenchSubsystem::Report()
{
    Samples.Sort();
    RebuildSamples.Sort();
    StepSamples.Sort();

    const int32 Count = Samples.Num();
    if (Count == 0)
    {
        UE_LOG(LogCrossBench, Error, TEXT("crossbench: no samples"));
        FPlatformMisc::RequestExitWithStatus(false, 1);
        return;
    }

    int32 OverBudget = 0;
    for (const double Sample : Samples)
    {
        OverBudget += Sample > BudgetMs ? 1 : 0;
    }

    const double Median = Percentile(Samples, 0.50);
    const FString Machine = MachineDescription(MachineOverride);

    const FString Line = FString::Printf(
        TEXT("crossbench {\"engine\":\"unreal\",\"engine_version\":\"%s\",\"workload\":\"W1-fixed-tick\","
             "\"agents\":%d,\"ticks\":%d,\"warmup\":%d,\"budget_ms\":%.1f,\"median_ms\":%.3f,\"p95_ms\":%.3f,"
             "\"p99_ms\":%.3f,\"max_ms\":%.3f,\"over_budget_pct\":%.2f,\"ns_per_agent\":%.0f,\"machine\":\"%s\","
             "\"notes\":\"subsystem tick: counting-sort grid rebuild plus seek and bounded separation; engine frame, "
             "rendering, physics and audio excluded\"}"),
        *FEngineVersion::Current().ToString(),
        Agents,
        Count,
        Warmup,
        BudgetMs,
        Median,
        Percentile(Samples, 0.95),
        Percentile(Samples, 0.99),
        Samples.Last(),
        100.0 * static_cast<double>(OverBudget) / static_cast<double>(Count),
        Median * 1000000.0 / static_cast<double>(Agents),
        *Machine);

    // Raw stdout with no log prefix, so the line is parseable without a regex that knows what an engine's log format
    // looks like.
    FPlatformMisc::LocalPrint(*FString::Printf(TEXT("%s\n"), *Line));

    UE_LOG(
        LogCrossBench,
        Display,
        TEXT("crossbench split: median rebuild %.3f ms, median steering %.3f ms"),
        Percentile(RebuildSamples, 0.50),
        Percentile(StepSamples, 0.50));

    FPlatformMisc::RequestExit(false);
}
