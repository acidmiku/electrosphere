//! Original command observations with explicit original math-call results.
//! This validates command mutations separately from the math implementation.
use ac3_mission::*;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, BufRead};

fn unhex(text: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return Err("invalid hexadecimal bytes".into());
    }
    (0..text.len())
        .step_by(2)
        .map(|offset| u8::from_str_radix(&text[offset..offset + 2], 16).map_err(Into::into))
        .collect()
}
fn bits(text: &str) -> Result<i32, Box<dyn std::error::Error>> {
    let value: i64 = text.parse()?;
    if value < i64::from(i32::MIN) || value > i64::from(u32::MAX) {
        return Err("invalid register value".into());
    }
    Ok(value as i32)
}
struct ObservedMath(RefCell<VecDeque<(BriefingMathRequest, i32)>>);
impl BriefingMath for ObservedMath {
    fn evaluate(&self, request: BriefingMathRequest) -> Result<i32, BriefingScriptError> {
        let next = self.0.borrow_mut().pop_front();
        if let Some((expected, result)) = next
            && expected == request
        {
            Ok(result)
        } else {
            Err(BriefingScriptError::MissingMath(request))
        }
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    for line in io::stdin().lock().lines() {
        let line = line?;
        let values: Vec<_> = line.split_whitespace().collect();
        if values.len() < 3 {
            return Err(
                "expected state_hex/script_base/command_hex and optional math records".into(),
            );
        }
        let mut bytes = unhex(values[0])?;
        let base: u32 = values[1].parse()?;
        let command = BriefingCommand::from_bytes(&unhex(values[2])?)?;
        let mut observations = VecDeque::new();
        for value in &values[3..] {
            let fields: Vec<_> = value.split(':').collect();
            if fields.len() != 4 {
                return Err("math observation must contain kind:a0:a1:v0".into());
            }
            let a = bits(fields[1])?;
            let b = bits(fields[2])?;
            let result = bits(fields[3])?;
            let request = match fields[0] {
                "atan2" => BriefingMathRequest::Atan2 { y: a, x: b },
                "sqrt" => BriefingMathRequest::SquareRoot { value: a as u32 },
                "sin" => BriefingMathRequest::Sin { angle: a },
                "cos" => BriefingMathRequest::Cos { angle: a },
                _ => return Err("unknown math request".into()),
            };
            observations.push_back((request, result));
        }
        let math = ObservedMath(RefCell::new(observations));
        let update = apply_briefing_command(
            BriefingScriptState::from_bytes(&bytes, base)?,
            command,
            &math,
        )?;
        if !math.0.borrow().is_empty() {
            return Err("unconsumed original math calls".into());
        }
        update.state.write_into(&mut bytes, base)?;
        for byte in bytes {
            print!("{byte:02x}");
        }
        for effect in update.effects {
            if let Effect::PlaySound { id } = effect {
                print!(" {id}");
            }
        }
        println!();
    }
    Ok(())
}
