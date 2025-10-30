use anyhow::{anyhow, Context, Result};
use regex::Regex;
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub struct OpenedFile {
    pub changelist: String,   // "12345" or "default"
    pub depot_file: String,   // //depot/...
    pub action: String,       // edit/add/delete/integrate/etc.
    pub workrev: Option<String>, // #<rev> (if present)
}

/// Run a command and return stdout as String.
fn run(cmd: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(cmd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("Failed to execute: {} {:?}", cmd, args))?;

    if !out.status.success() {
        let e = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("Command `{cmd} {args:?}` failed: {e}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Prefer tagged output for robust parsing.
pub fn get_opened_files() -> Result<Vec<OpenedFile>> {
    // `p4 -ztag opened` produces blocks like:
    // ... depotFile //depot/path/file
    // ... clientFile /work/path/file
    // ... rev 27
    // ... action edit
    // ... change 12345   (or) ... change default
    let stdout = run("p4", &["-ztag", "opened"])?;
    let line_re = Regex::new(r"^\.\.\.\s+(\w+)\s+(.+)$").unwrap();

    let mut current: OpenedFile = OpenedFile {
        changelist: "default".to_string(),
        depot_file: String::new(),
        action: String::new(),
        workrev: None,
    };
    let mut have_any = false;
    let mut out = Vec::new();

    for line in stdout.lines() {
        if let Some(cap) = line_re.captures(line) {
            let key = cap[1].to_string();
            let val = cap[2].to_string();

            match key.as_str() {
                "depotFile" => {
                    // starting a new record? push previous if it had data
                    if have_any {
                        out.push(current.clone());
                        current = OpenedFile {
                            changelist: "default".to_string(),
                            depot_file: String::new(),
                            action: String::new(),
                            workrev: None,
                        };
                    }
                    current.depot_file = val;
                    have_any = true;
                }
                "action" => current.action = val,
                "change" => current.changelist = val,
                "rev" => current.workrev = Some(val),
                _ => {}
            }
        }
    }
    if have_any {
        out.push(current);
    }
    Ok(out)
}
