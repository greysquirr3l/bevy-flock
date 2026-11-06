//! Groups: how many a frame can drive.
//!
//! A group decision is cheap; the question is how cheap, because a battle with three
//! hundred squads asks for three hundred decisions on the same cadence. The numbers
//! here are what the staggered cadence in `domain::groups` is tuned against.

use bevy_flock::domain::groups::{Formation, GroupStore};
use bevy_flock::domain::ids::{GroupId, NpcId};
use bevy_flock::testkit::OrDie;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

fn build(groups: usize, members: usize) -> (GroupStore, Vec<GroupId>) {
    let mut store = GroupStore::new();
    let mut ids = Vec::with_capacity(groups);
    let mut next = 0_u32;
    for index in 0..groups {
        let formation = match index % 4 {
            0 => Formation::Ring,
            1 => Formation::Wedge,
            2 => Formation::Column,
            _ => Formation::Free,
        };
        let leader = NpcId::unique(next);
        next += 1;
        let id = store.create(leader, formation).or_die();
        for _ in 1..members {
            store.join(id, NpcId::unique(next)).or_die();
            next += 1;
        }
        ids.push(id);
    }
    (store, ids)
}

fn bench_slots(c: &mut Criterion) {
    let mut group = c.benchmark_group("groups");
    for (groups, members) in [(10_usize, 8_usize), (100, 12), (1_000, 16)] {
        let (store, ids) = build(groups, members);
        let total = groups * members;
        group.throughput(Throughput::Elements(total as u64));
        group.bench_with_input(
            BenchmarkId::new("slot_pass", total),
            &total,
            |bencher, _| {
                bencher.iter(|| {
                    let mut sum = 0.0_f32;
                    for id in &ids {
                        let Some(group) = store.get(*id) else {
                            continue;
                        };
                        let count = group.len() as u16;
                        let formation = group.formation();
                        for (slot, _npc) in group.members().enumerate() {
                            if let Some(offset) = formation.slot_offset(slot as u16, count, 1.5) {
                                sum += offset.length();
                            }
                        }
                    }
                    std::hint::black_box(sum)
                });
            },
        );
    }
    group.finish();
}

fn bench_membership(c: &mut Criterion) {
    let mut group = c.benchmark_group("groups");
    let (mut store, ids) = build(100, 16);
    let target = *ids.first().or_die();
    let mut next = 900_000_u32;
    group.bench_function("join_and_leave", |bencher| {
        bencher.iter(|| {
            next = next.wrapping_add(1);
            let npc = NpcId::unique(next);
            let _ = store.join(target, npc);
            let _ = store.leave(target, npc);
        });
    });
    group.bench_function("despawn_reconciliation", |bencher| {
        let (mut store, ids) = build(50, 16);
        let mut index = 0_u32;
        bencher.iter(|| {
            let slot = index.checked_rem(ids.len() as u32).unwrap_or(0) as usize;
            index = index.wrapping_add(1);
            if let Some(id) = ids.get(slot).copied() {
                let member = store.get(id).and_then(|group| group.members().next());
                if let Some(member) = member {
                    let _ = store.on_despawn(member);
                }
            }
        });
    });
    group.finish();
}

criterion_group!(benches, bench_slots, bench_membership);
criterion_main!(benches);
