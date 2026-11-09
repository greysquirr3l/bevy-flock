// The Unreal side of docs/dev/tuning/benchmarks/cross-engine.md.
//
// Three engine modules are all the fixture needs: no Mass, no AIModule, no NavigationSystem. The point of the harness
// is a crowd loop written in the engine's own idiom, so a dependency list that dragged in the crowd plugin would
// measure something else.

using UnrealBuildTool;

public class CrossBench : ModuleRules
{
    public CrossBench(ReadOnlyTargetRules Target) : base(Target)
    {
        PCHUsage = PCHUsageMode.UseExplicitOrSharedPCHs;

        PublicDependencyModuleNames.AddRange(new string[] { "Core", "CoreUObject", "Engine" });
    }
}
