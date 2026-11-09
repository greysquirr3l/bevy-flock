// The Unreal side of docs/dev/tuning/benchmarks/cross-engine.md.
//
// A world subsystem, so the fixture needs no actor placed in a level and no blueprint: adding this module to a
// project is the whole setup. It reads its parameters from the command line, runs the W1 fixed-tick workload, prints
// one JSON line in the same shape the other two harnesses print, and exits.

#pragma once

#include "CoreMinimal.h"
#include "Subsystems/WorldSubsystem.h"
#include "Tickable.h"

#include "CrossBenchSubsystem.generated.h"

DECLARE_LOG_CATEGORY_EXTERN(LogCrossBench, Display, All);

/**
 * The W1 crowd fixture, measured inside the engine's own tick.
 *
 * The workload matches `examples/cross_bench.rs` and `benchmarks/godot/cross_bench.gd`: N agents in a 1 km cube
 * around the origin, one shared goal at the origin, a fixed 60 Hz step, a uniform-grid rebuild every tick and local
 * separation. The shape is SoA (`TArray<FVector>` of positions and velocities), not an array of `AActor*`, because an
 * array of actors measures UObject overhead rather than crowd simulation.
 */
UCLASS()
class CROSSBENCH_API UCrossBenchSubsystem : public UTickableWorldSubsystem
{
    GENERATED_BODY()

public:
    //~ Begin USubsystem
    virtual void Initialize(FSubsystemCollectionBase& Collection) override;
    //~ End USubsystem

    //~ Begin FTickableGameObject
    virtual void Tick(float DeltaTime) override;
    virtual TStatId GetStatId() const override;
    virtual bool IsTickable() const override { return bRunning; }
    //~ End FTickableGameObject

    //~ Begin UWorldSubsystem
    virtual bool DoesSupportWorldType(const EWorldType::Type WorldType) const override;
    //~ End UWorldSubsystem

private:
    /** One grid cell's linear index, clamped into the volume so a body on the boundary still has a cell. */
    int32 CellIndex(const FVector& Position) const;

    /** Counting sort into `Sorted` by cell, the same shape the Rust side uses: counts, prefix, scatter. */
    void RebuildGrid();

    /** Seek the shared goal, separate from a bounded number of same-cell neighbours, integrate, clamp. */
    void Step();

    /** Print the JSON line, then stop the process. */
    void Report();

    int32 Agents = 10000;
    int32 Ticks = 1000;
    int32 Warmup = 100;
    int32 Seed = 0xC0FFEE;
    double BudgetMs = 16.6;

    bool bRunning = false;
    int32 TickIndex = 0;

    TArray<FVector> Positions;
    TArray<FVector> Velocities;
    TArray<double> Samples;
    TArray<double> RebuildSamples;
    TArray<double> StepSamples;

    TArray<int32> CellStart;
    TArray<int32> CellCursor;
    TArray<int32> Sorted;

    FIntVector GridDims = FIntVector(1, 1, 1);
    double CellSize = 10.0;

    /** Set from `-crossbenchmachine=`, so a row can carry the exact machine string the other harnesses report. */
    FString MachineOverride;
};
