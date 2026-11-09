# The Godot side of docs/benchmarks/cross-engine.md.
#
# Attach to a Node in a Godot 4.6 project, set `agents`, and run headless:
#
#   godot --headless --quit-after 1200 res://main.tscn | grep crossbench
#
# It implements the same fixture as examples/cross_bench.rs: N agents in a 2D square, one shared goal, a fixed-tick loop
# that advances every agent, then the same six statistics in the same JSON shape. Pure GDScript, no nodes per agent — an
# array of vectors, which is what a fair comparison requires; instancing N Node2Ds would measure scene-tree overhead and
# is a different experiment.
extends Node

@export var agents: int = 10_000
@export var ticks: int = 1_000
@export var warmup: int = 100
@export var budget_ms: float = 16.6
@export var seed_value: int = 0xC0FFEE

const HALF: float = 500.0  # the 1 km cube's 2D footprint, same as the Rust fixture
const SPEED: float = 4.0

var _positions := PackedVector2Array()
var _velocities := PackedVector2Array()
var _samples := PackedFloat64Array()
var _tick := 0
var _goal := Vector2.ZERO


func _ready() -> void:
	var rng := RandomNumberGenerator.new()
	rng.seed = seed_value
	_positions.resize(agents)
	_velocities.resize(agents)
	for index in agents:
		_positions[index] = Vector2(rng.randf_range(-HALF, HALF), rng.randf_range(-HALF, HALF))
		var heading := Vector2.RIGHT.rotated(rng.randf_range(0.0, TAU))
		_velocities[index] = heading * SPEED


func _physics_process(delta: float) -> void:
	# Warm-up ticks are run but not measured, exactly as the Rust harness does.
	if _tick >= warmup + ticks:
		_report()
		set_physics_process(false)
		return

	var started := Time.get_ticks_usec()
	_step(delta)
	var elapsed_ms := float(Time.get_ticks_usec() - started) / 1000.0

	if _tick >= warmup:
		_samples.append(elapsed_ms)
	_tick += 1


func _step(delta: float) -> void:
	# The workload: steer toward a shared goal, integrate, keep inside the square. One pass over 10k agents, which is the
	# number the published Godot result spends 5 ms on.
	var count := _positions.size()
	for index in count:
		var position := _positions[index]
		var desired := (_goal - position).normalized() * SPEED
		var velocity := _velocities[index].lerp(desired, 0.05)
		_velocities[index] = velocity
		position += velocity * delta
		_positions[index] = Vector2(clampf(position.x, -HALF, HALF), clampf(position.y, -HALF, HALF))


func _report() -> void:
	_samples.sort()
	var count := _samples.size()
	if count == 0:
		push_error("crossbench: no samples")
		return
	var over := 0
	for sample in _samples:
		if sample > budget_ms:
			over += 1
	var median := _samples[count >> 1]
	print("crossbench ", JSON.stringify({
		"engine": "godot",
		"engine_version": Engine.get_version_info().string,
		"workload": "W1-fixed-tick",
		"agents": agents,
		"ticks": count,
		"warmup": warmup,
		"budget_ms": budget_ms,
		"median_ms": snappedf(median, 0.001),
		"p95_ms": snappedf(_percentile(0.95), 0.001),
		"p99_ms": snappedf(_percentile(0.99), 0.001),
		"max_ms": snappedf(_samples[count - 1], 0.001),
		"over_budget_pct": snappedf(float(over) * 100.0 / float(count), 0.01),
		"ns_per_agent": roundi(median * 1_000_000.0 / float(agents)),
		"machine": OS.get_processor_name() + ", " + str(OS.get_processor_count()) + " threads",
		"notes": "_physics_process over an array of vectors; no rendering, animation, physics bodies or audio"
	}))


func _percentile(fraction: float) -> float:
	var rank := int(ceil(fraction * float(_samples.size())))
	var index := clampi(rank - 1, 0, _samples.size() - 1)
	return _samples[index]
