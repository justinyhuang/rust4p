use anyhow::{anyhow, Context, Result};
use regex::Regex;
use std::io::Write;
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

/// Get changelist description. Returns None if CL doesn't exist.
pub fn get_change_description(cl_number: &str) -> Result<Option<String>> {
    let output = Command::new("p4")
        .arg("change")
        .arg("-o")
        .arg(cl_number)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("Failed to execute p4 change -o {}", cl_number))?;
    
    if !output.status.success() {
        // CL doesn't exist
        return Ok(None);
    }
    
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut description = String::new();
    let mut in_description = false;
    
    for line in stdout.lines() {
        if line.starts_with("Description:") {
            in_description = true;
            continue;
        }
        if in_description {
            if line.starts_with('\t') || line.starts_with("    ") {
                description.push_str(line.trim());
                description.push('\n');
            } else {
                break;
            }
        }
    }
    
    Ok(Some(description.trim().to_string()))
}

/// Create a new changelist. Returns the CL number.
pub fn create_changelist() -> Result<String> {
    let output = Command::new("p4")
        .arg("change")
        .arg("-o")
        .stdout(Stdio::piped())
        .output()
        .context("Failed to get changelist template")?;
    
    if !output.status.success() {
        anyhow::bail!("Failed to get changelist template");
    }
    
    let template = String::from_utf8_lossy(&output.stdout);
    let mut modified = String::new();
    
    for line in template.lines() {
        if line.starts_with("Change:") {
            modified.push_str("Change:\tnew\n");
        } else if line.starts_with("Description:") {
            modified.push_str("Description:\n\t<enter description here>\n");
        } else {
            modified.push_str(line);
            modified.push('\n');
        }
    }
    
    let mut child = Command::new("p4")
        .arg("change")
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to spawn p4 change -i")?;
    
    child.stdin.as_mut().unwrap().write_all(modified.as_bytes())?;
    
    let output = child.wait_with_output()?;
    
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to create changelist: {}", err);
    }
    
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Parse "Change 12345 created."
    let re = Regex::new(r"Change (\d+) created").unwrap();
    if let Some(cap) = re.captures(&stdout) {
        Ok(cap[1].to_string())
    } else {
        anyhow::bail!("Failed to parse CL number from: {}", stdout);
    }
}

/// Unshelve files from a changelist
pub fn unshelve_changelist(cl_number: &str) -> Result<()> {
    let output = Command::new("p4")
        .arg("unshelve")
        .arg("-s")
        .arg(cl_number)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("Failed to unshelve CL {}", cl_number))?;
    
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to unshelve: {}", err);
    }
    
    Ok(())
}
