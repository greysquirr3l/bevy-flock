# The Godot side of docs/dev/tuning/benchmarks/cross-engine.md.
#
# The project in this directory is committed, so there is no setup step:
#
#   godot --headless --path benchmarks/godot -- --agents 10000 --ticks 1000
#
# Parameters come from the command line because a headless run cannot touch the inspector, and because the fixture only
# means something if both engines run the same numbers without anyone editing a scene. Accepted, in `--key value` or
# `--key=value` form: agents, ticks, warmup, budget_ms, seed. Anything else is reported and ignored.
#
# It implements the same fixture as examples/cross_bench.rs: N agents in a 2D square, one shared goal, a fixed-tick loop
# that advances every agent, then the same six statistics in the same JSON shape. Pure GDScript, no nodes per agent: an
# array of vectors, which is what a fair comparison requires. Instancing N Node2Ds would measure scene-tree overhead and
# is a different experiment.
extends Node

@export var agents: int = 10_000
@export var ticks: int = 1_000
@export var warmup: int = 100
@export var budget_ms: float = 16.6
@export var seed_value: int = 0xC0FFEE
# "array": one pass over the agents, no index. "grid": the same pass plus a uniform-grid rebuild,
# which is the work the Rust side pays for, so the two rows measure the same fixture.
@export var mode: String = "array"

const HALF: float = 500.0  # the 1 km cube's 2D footprint, same as the Rust fixture
const SPEED: float = 4.0

var _positions := PackedVector2Array()
var _velocities := PackedVector2Array()
var _samples := PackedFloat64Array()
var _tick := 0
var _goal := Vector2.ZERO
var _cells := {}
const CELL: float = 10.0  # metres, in the same range the Rust grid picks for this population


func _ready() -> void:
	_apply_args()
	var rng := RandomNumberGenerator.new()
	rng.seed = seed_value
	_positions.resize(agents)
	_velocities.resize(agents)
	for index in agents:
		_positions[index] = Vector2(rng.randf_range(-HALF, HALF), rng.randf_range(-HALF, HALF))
		var heading := Vector2.RIGHT.rotated(rng.randf_range(0.0, TAU))
		_velocities[index] = heading * SPEED


func _apply_args() -> void:
	var argv := OS.get_cmdline_user_args()
	var index := 0
	while index < argv.size():
		var argument: String = argv[index]
		if not argument.begins_with("--"):
			push_warning("crossbench: ignoring %s" % argument)
			index += 1
			continue
		var key := argument.substr(2)
		var value := ""
		if key.contains("="):
			var halves := key.split("=", true, 1)
			key = halves[0]
			value = halves[1]
		elif index + 1 < argv.size() and not argv[index + 1].begins_with("--"):
			value = argv[index + 1]
			index += 1
		match key:
			"agents":
				agents = maxi(int(value), 1)
			"ticks":
				ticks = maxi(int(value), 1)
			"warmup":
				warmup = maxi(int(value), 0)
			"budget_ms":
				budget_ms = float(value)
			"seed":
				seed_value = int(value)
			"mode":
				mode = value
			_:
				push_warning("crossbench: ignoring --%s" % key)
		index += 1


func _physics_process(delta: float) -> void:
	# Warm-up ticks are run but not measured, exactly as the Rust harness does.
	if _tick >= warmup + ticks:
		_report()
		set_physics_process(false)
		return

	var started := Time.get_ticks_usec()
	if mode == "grid":
		_rebuild_grid()
	_step(delta)
	var elapsed_ms := float(Time.get_ticks_usec() - started) / 1000.0

	if _tick >= warmup:
		_samples.append(elapsed_ms)
	_tick += 1


func _rebuild_grid() -> void:
	# The cost the Rust side pays every tick and the array mode does not: clear, bucket every agent, then read
	# each agent's own cell back. Dictionaries of Arrays are how this is written in GDScript, and the allocation
	# they do per occupied cell per tick is part of the measurement rather than an implementation detail.
	_cells.clear()
	var count := _positions.size()
	for index in count:
		var position := _positions[index]
		var key := Vector2i(floori(position.x / CELL), floori(position.y / CELL))
		if _cells.has(key):
			_cells[key].append(index)
		else:
			_cells[key] = [index]
	var occupied := 0
	for key in _cells:
		occupied += _cells[key].size()
	_grid_agents = occupied


var _grid_agents: int = 0


func _step(delta: float) -> void:
	# The workload: steer toward a shared goal, integrate, keep inside the square. One pass over every agent, which is
	# the cost the published Godot result attributes to its field update.
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
		get_tree().quit(1)
		return
	var over := 0
	for sample in _samples:
		if sample > budget_ms:
			over += 1
	var median := _samples[count >> 1]
	print("crossbench ", JSON.stringify({
		# The variant belongs in the name: two rows called "godot" at the same agent count, measuring
		# different work, make an ambiguous table.
		"engine": "godot-" + mode,
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
		"machine": _machine(),
		"notes": _notes(),
	}))
	# Report once and stop. No --quit-after is needed, so a run cannot be truncated mid-sample.
	get_tree().quit()


func _machine() -> String:
	# The same string the Rust harness builds, fallback included, so the comparator's same-machine check
	# compares identities rather than two different ways of saying "I could not read a CPU model".
	var cpu := OS.get_processor_name()
	if cpu.is_empty():
		cpu = "unknown cpu"
	return cpu + ", " + str(OS.get_processor_count()) + " threads"


func _notes() -> String:
	var suffix := "; no rendering, animation, physics bodies or audio"
	if mode == "grid":
		return ("_physics_process over a uniform grid rebuild plus steering at a fixed 60 Hz delta" + suffix)
	return ("_physics_process over an array of vectors at a fixed 60 Hz delta" + suffix)


func _percentile(fraction: float) -> float:
	var rank := int(ceil(fraction * float(_samples.size())))
	var index := clampi(rank - 1, 0, _samples.size() - 1)
	return _samples[index]
