//! Release-build evidence for the narrow local-object allocation-elimination pass.
//! Timings are informational only and are never asserted by tests.

use std::time::Instant;

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, PreparedSequentialExecution};
use aster_compiler::compile;
use aster_mir as mir;

const SAMPLES: usize = 5;

#[derive(Clone, Copy)]
enum Shape {
    Tiny,
    MultiField,
    Mixed,
    Alias,
    Call,
    HiddenBacking,
}

impl Shape {
    fn name(self) -> &'static str {
        match self {
            Self::Tiny => "tiny",
            Self::MultiField => "multi-field",
            Self::Mixed => "mixed-occasional",
            Self::Alias => "alias-negative",
            Self::Call => "call-negative",
            Self::HiddenBacking => "aarm-hidden",
        }
    }

    fn source(self, iterations: usize) -> String {
        match self {
            Self::Tiny => format!(
                "public class Box {{ public int value; }} \
                 public int Run() {{ int total = 0; for (int i = 0; i < {iterations}; i++) {{ \
                 Box box = new Box(); box.value = 1; total += box.value; }} return total; }}"
            ),
            Self::MultiField => format!(
                "public class Pair {{ public int left; public int right; }} \
                 public int Run() {{ int total = 0; for (int i = 0; i < {iterations}; i++) {{ \
                 Pair pair = new Pair(); pair.left = 1; pair.right = 2; \
                 total += pair.left + pair.right; }} return total; }}"
            ),
            Self::Mixed => format!(
                "public class Box {{ public int value; }} \
                 public int Run() {{ int total = 0; for (int i = 0; i < {iterations}; i++) {{ \
                 total += i % 7; if (i % 16 == 0) {{ Box box = new Box(); box.value = 5; \
                 total += box.value; }} }} return total; }}"
            ),
            Self::Alias => format!(
                "public class Box {{ public int value; }} \
                 public int Run() {{ int total = 0; for (int i = 0; i < {iterations}; i++) {{ \
                 Box box = new Box(); Box alias = box; alias.value = 1; total += box.value; \
                 }} return total; }}"
            ),
            Self::Call => format!(
                "public class Box {{ public int value; }} \
                 internal int Read(Box box) {{ return box.value; }} \
                 public int Run() {{ int total = 0; for (int i = 0; i < {iterations}; i++) {{ \
                 Box box = new Box(); box.value = 1; total += Read(box); }} return total; }}"
            ),
            Self::HiddenBacking => format!(
                "public class Pair {{ public int left; public int right; }} \
                 public int Run() {{ int total = 0; for (int i = 0; i < {iterations}; i++) {{ \
                 Pair pair = new Pair(); pair.left = i; pair.right = 1; \
                 List<int> values = new List<int>(); values.Add(i); \
                 total += pair.right; }} return total; }}"
            ),
        }
    }

    fn expected(self, iterations: usize) -> i32 {
        let iterations = i32::try_from(iterations).expect("benchmark iteration count fits int");
        match self {
            Self::Tiny | Self::Alias | Self::Call | Self::HiddenBacking => iterations,
            Self::MultiField => iterations * 3,
            Self::Mixed => {
                let scalar = (0..iterations).map(|value| value % 7).sum::<i32>();
                let objects = ((iterations + 15) / 16) * 5;
                scalar + objects
            }
        }
    }
}

fn marker_count(module: &mir::Module) -> usize {
    module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                instruction,
                mir::Instruction::TemporarySubregionEnter { .. }
                    | mir::Instruction::TemporarySubregionExit { .. }
            )
        })
        .count()
}

fn run(shape: Shape, iterations: usize) -> (f64, MemoryStats, usize, u64) {
    let compilation = compile(&shape.source(iterations)).expect("benchmark source compiles");
    let expected = ExecutionValue::Int(shape.expected(iterations));
    let prepared = PreparedSequentialExecution::prepare(&compilation.mir, "Run")
        .expect("benchmark JIT preparation succeeds");
    assert_eq!(prepared.invoke().expect("warmup executes"), expected);
    let (value, stats) = prepared
        .invoke_with_stats()
        .expect("benchmark source executes");
    assert_eq!(value, expected);

    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        let value = prepared.invoke().expect("benchmark source executes");
        samples.push(start.elapsed().as_secs_f64() * 1_000.0);
        assert_eq!(value, expected);
    }
    samples.sort_by(f64::total_cmp);
    #[cfg(feature = "aarm-telemetry")]
    let dynamic_regions = {
        let (value, telemetry) = prepared
            .invoke_with_aarm_telemetry()
            .expect("benchmark telemetry executes");
        assert_eq!(value, expected);
        telemetry.temporary.events.rewind_events.saturating_sub(1)
    };
    #[cfg(not(feature = "aarm-telemetry"))]
    let dynamic_regions = 0;
    (
        samples[SAMPLES / 2],
        stats,
        marker_count(&compilation.mir),
        dynamic_regions,
    )
}

fn main() {
    for iterations in [100_000, 1_000_000] {
        for shape in [
            Shape::Tiny,
            Shape::MultiField,
            Shape::Mixed,
            Shape::Alias,
            Shape::Call,
            Shape::HiddenBacking,
        ] {
            let (median_ms, stats, static_markers, dynamic_regions) = run(shape, iterations);
            println!(
                "shape={:<16} iterations={iterations:<7} median_ms={median_ms:>9.3} checksum={:>8} allocations={:>8} object_allocations={:>8} requested_bytes={:>10} peak_used_bytes={:>10} capacity_bytes={:>10} static_markers={static_markers:>2} dynamic_regions={dynamic_regions:>8}",
                shape.name(),
                shape.expected(iterations),
                stats.total_allocations,
                stats.object_allocations,
                stats.requested_bytes,
                stats.peak_used_bytes,
                stats.reserved_bytes,
            );
        }
    }
}
