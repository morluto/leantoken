pub(crate) mod indexing_repository;
pub(crate) mod protocol;

use leantoken_test_support::FixtureCase;
use serde::Serialize;
use std::{fmt::Debug, fs};

pub(crate) fn compare_or_bless<T>(
    case: &FixtureCase,
    expected: &T,
    actual: &T,
    bless: bool,
) -> Result<(), String>
where
    T: Debug + PartialEq + Serialize,
{
    if bless {
        let rendered = serde_json::to_string_pretty(actual).map_err(|error| error.to_string())?;
        fs::write(&case.expected, format!("{rendered}\n"))
            .map_err(|error| format!("write blessed expectation: {error}"))?;
        println!("blessed {}: {:?} -> {:?}", case.identity, expected, actual);
        return Ok(());
    }
    if expected != actual {
        return Err(format!(
            "semantic fixture mismatch for {}: expected {:?}, actual {:?}",
            case.identity, expected, actual
        ));
    }
    println!("fixture passed: {}", case.identity);
    Ok(())
}
