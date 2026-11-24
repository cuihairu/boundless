//! Matrix / vector / quaternion baselines (perf bench batch 1).
//!
//! The reference's scalar fallback path in `lib/math/` is what Linux
//! servers build, so these benches measure exactly that port: the 4x4
//! f32 multiply, the point transform, the vector normalise / lerp / dot
//! chain and the quaternion multiply / slerp chain. Fixtures are built
//! from index arithmetic — no randomness, so every run compares.

use std::hint::black_box;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use foundation::math::{Matrix, Quaternion, Vector3};

/// A deterministic matrix: every element is a distinct small value, so
/// no multiply turns into a trivial 0 or 1 product.
fn fixture_matrix(seed: u32) -> Matrix {
    let mut m = Matrix::zero();
    for (row, line) in m.m.iter_mut().enumerate() {
        for (col, cell) in line.iter_mut().enumerate() {
            let v = ((seed + (row * 4 + col) as u32) % 19) as f32 - 9.0;
            *cell = if v == 0.0 { 2.5 } else { v };
        }
    }
    m
}

/// 256 deterministic points; the transform runs over all of them per
/// iteration, like a batched scene-graph pass.
fn fixture_points() -> Vec<Vector3> {
    (0..256)
        .map(|i| {
            Vector3::new(
                (i % 13) as f32 + 0.5,
                (i % 7) as f32 - 1.0,
                (i % 11) as f32 + 0.25,
            )
        })
        .collect()
}

fn bench_matrix_multiply(c: &mut Criterion) {
    let a = fixture_matrix(0);
    let b = fixture_matrix(7);
    let mut group = c.benchmark_group("matrix");
    group.throughput(Throughput::Elements(16));
    group.bench_function("multiply_4x4_f32", |bench| {
        bench.iter_batched(
            || fixture_matrix(3),
            |mut m| {
                m.multiply(black_box(&a), black_box(&b));
                black_box(m)
            },
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

fn bench_matrix_apply_point(c: &mut Criterion) {
    let m = fixture_matrix(2);
    let points = fixture_points();
    let mut group = c.benchmark_group("matrix");
    group.throughput(Throughput::Elements(points.len() as u64));
    group.bench_function("apply_point_x256", |bench| {
        bench.iter(|| {
            for point in &points {
                black_box(m.apply_point(black_box(point)));
            }
        })
    });
    group.finish();
}

fn bench_vector_ops(c: &mut Criterion) {
    let points = fixture_points();
    let mut group = c.benchmark_group("vector3");
    group.throughput(Throughput::Elements(points.len() as u64));
    group.bench_function("normalise_lerp_dot_x256", |bench| {
        bench.iter(|| {
            let mut acc = 0.0f32;
            for pair in points.windows(2) {
                let mut a = pair[0];
                a.normalise();
                let lerped = Vector3::lerp(&a, &pair[1], 0.37);
                acc += lerped.dot_product(&pair[1]);
            }
            black_box(acc)
        })
    });
    group.finish();
}

fn bench_quaternion_ops(c: &mut Criterion) {
    let axis = Vector3::new(0.3, -0.5, 0.8);
    let q_a = Quaternion::from_angle_axis(0.42, &axis);
    let q_b = Quaternion::from_angle_axis(-1.1, &Vector3::new(0.0, 1.0, 0.0));
    c.bench_function("quaternion_multiply_slerp", |bench| {
        bench.iter(|| {
            let mut acc = 0.0f32;
            let mut current = q_a;
            for i in 0..64 {
                current = Quaternion::multiply(&current, black_box(&q_b));
                let blended = Quaternion::slerp(&current, &q_b, (i % 8) as f32 / 8.0);
                acc += blended.dot_product(&current);
            }
            black_box(acc)
        })
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(100);
    targets = bench_matrix_multiply, bench_matrix_apply_point, bench_vector_ops, bench_quaternion_ops
}
criterion_main!(benches);
