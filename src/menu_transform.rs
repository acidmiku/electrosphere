//! Native rectangle animation from overlay 2 entry 0x800EF21C.
//!
//! The eight signed halfwords are [index, mode, pivot_x, pivot_y, stage0,
//! stage1, stage2, timer]. Index is not read by this routine. Inputs are copied;
//! arithmetic traps return errors without exposing a partially modified rect.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransformedRect {
    pub draw: bool,
    /// Signed x, y, width and height; negative dimensions remain legal here.
    pub rect: [i16; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformError {
    DivisionByZero(&'static str),
    DivisionOverflow(&'static str),
}

impl fmt::Display for TransformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DivisionByZero(operation) => {
                write!(f, "rectangle animation divides by zero: {operation}")
            }
            Self::DivisionOverflow(operation) => {
                write!(f, "rectangle animation division overflows: {operation}")
            }
        }
    }
}
impl std::error::Error for TransformError {}

fn quotient(
    numerator: i32,
    denominator: i32,
    operation: &'static str,
) -> Result<i32, TransformError> {
    if denominator == 0 {
        return Err(TransformError::DivisionByZero(operation));
    }
    numerator
        .checked_div(denominator)
        .ok_or(TransformError::DivisionOverflow(operation))
}

fn scaled(
    value: i32,
    elapsed: i32,
    duration: i32,
    operation: &'static str,
) -> Result<i16, TransformError> {
    // MULT/MFLO keeps only the low word before signed DIV. SH keeps its low half.
    Ok(quotient(value.wrapping_mul(elapsed), duration, operation)? as i16)
}

fn position(
    value: i16,
    pivot: i16,
    elapsed: i32,
    duration: i32,
    operation: &'static str,
) -> Result<i16, TransformError> {
    Ok(pivot.wrapping_add(scaled(
        i32::from(value) - i32::from(pivot),
        elapsed,
        duration,
        operation,
    )?))
}

/// Transform a copied rectangle, preserving MIPS low-word products, signed
/// truncating division, low-halfword stores and the original branch boundaries.
/// There is no threshold ordering check or geometric clamp. Unknown modes
/// (including negative values) return the unchanged rectangle with `draw=true`.
pub fn transform_rect(
    mut rect: [i16; 4],
    record: [i16; 8],
) -> Result<TransformedRect, TransformError> {
    let [_, mode, px, py, start, middle, end, timer] = record;
    let (start, middle, end, timer) = (
        i32::from(start),
        i32::from(middle),
        i32::from(end),
        i32::from(timer),
    );
    let mut draw = true;
    match mode {
        1 => {
            if timer <= start {
                draw = false;
            } else if timer <= middle {
                rect[0] = position(
                    rect[0],
                    px,
                    timer - start,
                    middle - start,
                    "mode 1 initial x",
                )?;
                let dy = i32::from(rect[1]) - i32::from(py);
                rect[1] = if dy == 0 {
                    py
                } else {
                    py.wrapping_add(quotient(dy, dy.abs(), "mode 1 initial y direction")? as i16)
                };
                rect[2] = scaled(
                    i32::from(rect[2]),
                    timer - start,
                    middle - start,
                    "mode 1 initial width",
                )?;
                rect[3] = 2;
            } else if timer < end {
                rect[1] = position(rect[1], py, timer - middle, end - middle, "mode 1 final y")?;
                rect[3] = scaled(
                    i32::from(rect[3]),
                    timer - middle,
                    end - middle,
                    "mode 1 final height",
                )?;
            }
        }
        2 => {
            if timer <= start {
                draw = false;
            } else if timer < middle {
                let dx = i32::from(rect[0]) - i32::from(px);
                // Unlike mode 1's y direction, equal x and pivot really traps.
                rect[0] =
                    px.wrapping_add(quotient(dx, dx.abs(), "mode 2 initial x direction")? as i16);
                rect[1] = position(
                    rect[1],
                    py,
                    timer - start,
                    middle - start,
                    "mode 2 initial y",
                )?;
                rect[2] = 1;
                rect[3] = scaled(
                    i32::from(rect[3]),
                    timer - start,
                    middle - start,
                    "mode 2 initial height",
                )?;
            } else if timer < end {
                rect[0] = position(rect[0], px, timer - middle, end - middle, "mode 2 final x")?;
                rect[2] = scaled(
                    i32::from(rect[2]),
                    timer - middle,
                    end - middle,
                    "mode 2 final width",
                )?;
            }
        }
        3 => {
            if timer <= start {
                draw = false;
            } else if timer < middle {
                rect[0] = position(rect[0], px, timer - start, middle - start, "mode 3 x")?;
                rect[1] = position(rect[1], py, timer - start, middle - start, "mode 3 y")?;
                // Assembly 800EF79C..7B0 uses stage2 for width only.
                rect[2] = scaled(
                    i32::from(rect[2]),
                    timer - start,
                    end - start,
                    "mode 3 width",
                )?;
                rect[3] = scaled(
                    i32::from(rect[3]),
                    timer - start,
                    middle - start,
                    "mode 3 height",
                )?;
            }
        }
        4 => draw = end <= timer,
        _ => {}
    }
    Ok(TransformedRect { draw, rect })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECT: [i16; 4] = [100, 200, 80, 60];
    fn at(mode: i16, timer: i16) -> TransformedRect {
        transform_rect(RECT, [123, mode, 20, 40, 2, 6, 10, timer]).unwrap()
    }

    #[test]
    fn mode_one_boundary_includes_middle_in_flat_horizontal_phase() {
        assert_eq!(
            at(1, 2),
            TransformedRect {
                draw: false,
                rect: RECT
            }
        );
        assert_eq!(at(1, 3).rect, [40, 41, 20, 2]);
        assert_eq!(at(1, 6).rect, [100, 41, 80, 2]);
        assert_eq!(at(1, 7).rect, [100, 80, 80, 15]);
        assert_eq!(at(1, 10).rect, RECT);
        assert_eq!(
            transform_rect([100, 40, 80, 60], [0, 1, 20, 40, 2, 6, 10, 3])
                .unwrap()
                .rect,
            [40, 40, 20, 2]
        );
    }

    #[test]
    fn mode_two_middle_starts_horizontal_expansion_at_zero_width() {
        assert!(!at(2, 2).draw);
        assert_eq!(at(2, 3).rect, [21, 80, 1, 15]);
        assert_eq!(at(2, 6).rect, [20, 200, 0, 60]);
        assert_eq!(at(2, 7).rect, [40, 200, 20, 60]);
        assert_eq!(at(2, 10).rect, RECT);
        assert!(matches!(
            transform_rect([20, 200, 80, 60], [0, 2, 20, 40, 2, 6, 10, 3]),
            Err(TransformError::DivisionByZero("mode 2 initial x direction"))
        ));
    }

    #[test]
    fn mode_three_width_uses_end_threshold_but_animation_ends_at_middle() {
        assert!(!at(3, 2).draw);
        assert_eq!(at(3, 3).rect, [40, 80, 10, 15]);
        assert_eq!(at(3, 5).rect, [80, 160, 30, 45]);
        assert_eq!(at(3, 6).rect, RECT);
        assert_eq!(
            transform_rect(RECT, [0, 3, 20, 40, 2, 10, 0, 5])
                .unwrap()
                .rect,
            [50, 100, -120, 22]
        );
        assert!(matches!(
            transform_rect(RECT, [0, 3, 20, 40, 2, 10, 2, 5]),
            Err(TransformError::DivisionByZero("mode 3 width"))
        ));
        // A zero divisor in a branch never taken is not an error.
        assert_eq!(
            transform_rect(RECT, [0, 3, 20, 40, 2, 10, 2, 10])
                .unwrap()
                .rect,
            RECT
        );
    }

    #[test]
    fn signed_division_truncates_toward_zero_and_mflo_sh_wrap() {
        assert_eq!(
            transform_rect([-19, -31, -17, -23], [0, 3, 7, 9, 0, 7, 13, 1])
                .unwrap()
                .rect,
            [4, 4, -1, -3]
        );
        assert_eq!(
            transform_rect(
                [32767, 32767, 32767, -32768],
                [0, 3, -32768, -32768, -32768, 32767, 32767, 32766]
            )
            .unwrap()
            .rect,
            [32765, 32765, 32766, -32767]
        );
        assert_eq!(
            transform_rect([100, 200, 32767, 60], [0, 3, 20, 40, 0, 32767, 1, 32766])
                .unwrap()
                .rect[2],
            -32766
        );
        assert_eq!(
            quotient(i32::MIN, -1, "checked guard"),
            Err(TransformError::DivisionOverflow("checked guard"))
        );
    }

    #[test]
    fn delayed_visibility_passthrough_modes_and_unused_index() {
        assert!(!at(4, 9).draw);
        assert_eq!(
            at(4, 10),
            TransformedRect {
                draw: true,
                rect: RECT
            }
        );
        for mode in [i16::MIN, -1, 0, 5, i16::MAX] {
            assert_eq!(
                at(mode, 2),
                TransformedRect {
                    draw: true,
                    rect: RECT
                }
            );
        }
        assert_eq!(
            transform_rect(RECT, [i16::MIN, 1, 20, 40, 2, 6, 10, 3]).unwrap(),
            at(1, 3)
        );
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and original menu-transform oracle CSV"]
    fn original_overlay_leaf_vectors_match() {
        let root =
            std::path::PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("set AC3_DATA_DIR"));
        let csv = std::fs::read_to_string(root.join("analysis/menu-transform/oracle.csv")).unwrap();
        let mut count = 0;
        for line in csv
            .lines()
            .filter(|l| l.starts_with('-') || l.starts_with(|c: char| c.is_ascii_digit()))
        {
            let values: Vec<i32> = line.split(',').map(|v| v.parse().unwrap()).collect();
            assert_eq!(values.len(), 17);
            let rect = std::array::from_fn(|i| i16::try_from(values[i]).unwrap());
            let record = std::array::from_fn(|i| i16::try_from(values[i + 4]).unwrap());
            let expected = TransformedRect {
                draw: values[12] != 0,
                rect: std::array::from_fn(|i| i16::try_from(values[i + 13]).unwrap()),
            };
            assert_eq!(
                transform_rect(rect, record).unwrap(),
                expected,
                "original case {count}"
            );
            count += 1;
        }
        assert_eq!(count, 77);
    }
}
