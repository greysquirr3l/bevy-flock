// The Unreal side of docs/dev/tuning/benchmarks/cross-engine.md.
//
// The editor target exists so the project opens normally for anyone who wants to watch the fixture run; the benchmark
// itself is a `-game -nullrhi` run and never loads the editor.

using UnrealBuildTool;
using System.Collections.Generic;

public class CrossBenchEditorTarget : TargetRules
{
    public CrossBenchEditorTarget(TargetInfo Target) : base(Target)
    {
        Type = TargetType.Editor;
        DefaultBuildSettings = BuildSettingsVersion.Latest;
        IncludeOrderVersion = EngineIncludeOrderVersion.Latest;
        ExtraModuleNames.Add("CrossBench");
    }
}
