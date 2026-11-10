//! Litter mode: an agent that leaves its sandbox as hard to remove as it
//! can make it (PA-2: nothing a task was given or made outlives it).
//!
//! Started with a prompt like `litter` or `litter 50000`, the probe leaves
//! folders nested that many deep (50,000 unless said) in its scratch space.
//! Removing a folder with a call per level overflows the stack long before
//! that, and a stack overflow in glassd ends every task, not this one.
//!
//! It ends Done and says what it left. Whether that could be removed is
//! not this program's to see: the end-to-end suite checks that the task
//! ended, that glassd still answers, and that no sandbox is left behind.

use std::path::Path;

use glass_agent_sdk::Agent;

const DEFAULT_DEPTH: usize = 50_000;
/// More than anyone needs to make the point, and a bound on the run.
const MOST_DEPTH: usize = 200_000;

/// `litter 50000` -> 50,000 folders deep; anything else -> the default.
pub fn depth(prompt: &str) -> usize {
    prompt
        .split_whitespace()
        .find_map(|word| word.parse::<usize>().ok())
        .filter(|n| (1..=MOST_DEPTH).contains(n))
        .unwrap_or(DEFAULT_DEPTH)
}

/// Makes `depth` folders under `top`, each inside the last. One step at a
/// time from inside it: the whole path is far longer than a path may be.
fn nest(top: &Path, depth: usize) -> std::io::Result<()> {
    std::fs::create_dir(top)?;
    std::env::set_current_dir(top)?;
    for _ in 0..depth {
        std::fs::create_dir("d")?;
        std::env::set_current_dir("d")?;
    }
    std::fs::write("at-the-bottom", b"x")
}

pub fn run(agent: &Agent) -> Result<String, String> {
    let depth = depth(&agent.prompt);
    let work = agent.work_dir.clone();
    let nested = nest(&work.join("deep"), depth);
    // Back where a task's process starts, whatever happened.
    let _ = std::env::set_current_dir(&work);
    nested.map_err(|e| format!("couldn't nest {depth} folders: {e}"))?;
    Ok(format!("Left a tree {depth} folders deep"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_says_how_deep() {
        assert_eq!(depth("litter"), DEFAULT_DEPTH);
        assert_eq!(depth("litter 300"), 300);
        assert_eq!(depth("litter 0"), DEFAULT_DEPTH);
        assert_eq!(depth("litter 999999999"), DEFAULT_DEPTH);
    }
}
