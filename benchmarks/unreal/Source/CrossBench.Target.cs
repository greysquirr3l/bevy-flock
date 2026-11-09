// The Unreal side of docs/dev/tuning/benchmarks/cross-engine.md.

using UnrealBuildTool;
using System.Collections.Generic;

public class CrossBenchTarget : TargetRules
{
    public CrossBenchTarget(TargetInfo Target) : base(Target)
    {
        Type = TargetType.Game;
        DefaultBuildSettings = BuildSettingsVersion.Latest;
        IncludeOrderVersion = EngineIncludeOrderVersion.Latest;
        ExtraModuleNames.Add("CrossBench");
    }
}
