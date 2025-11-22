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
    // `p4 -ztag opened` produces blocks with tagged output fields
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

/// Get shelved files from a changelist
pub fn get_shelved_files(cl_number: &str) -> Result<Vec<OpenedFile>> {
    let stdout = run("p4", &["-ztag", "describe", "-S", "-s", cl_number])?;
    let line_re = Regex::new(r"^\.\.\.\s+(\w+?)(\d*)\s+(.+)$").unwrap();
    
    let mut files_map: std::collections::HashMap<usize, (Option<String>, Option<String>)> = std::collections::HashMap::new();
    
    for line in stdout.lines() {
        if let Some(cap) = line_re.captures(line) {
            let key = &cap[1];
            let index_str = &cap[2];
            let val = cap[3].to_string();
            
            let index = if index_str.is_empty() {
                0
            } else {
                index_str.parse::<usize>().unwrap_or(0)
            };
            
            let entry = files_map.entry(index).or_insert((None, None));
            
            match key {
                "depotFile" => {
                    entry.0 = Some(val);
                }
                "action" => {
                    entry.1 = Some(val);
                }
                _ => {}
            }
        }
    }
    
    // Convert to vector of OpenedFile
    let mut files = Vec::new();
    let mut indices: Vec<_> = files_map.keys().copied().collect();
    indices.sort();
    
    for idx in indices {
        if let Some((Some(file), Some(action))) = files_map.get(&idx) {
            files.push(OpenedFile {
                changelist: cl_number.to_string(),
                depot_file: file.clone(),
                action: action.clone(),
                workrev: None,
            });
        }
    }
    
    Ok(files)
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

/// Unshelve specific files from a changelist
pub fn unshelve_files(cl_number: &str, files: &[String]) -> Result<()> {
    let mut cmd = Command::new("p4");
    cmd.arg("unshelve")
        .arg("-s")
        .arg(cl_number);
    
    for file in files {
        cmd.arg(file);
    }
    
    let output = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("Failed to unshelve files from CL {}", cl_number))?;
    
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to unshelve: {}", err);
    }
    
    Ok(())
}

/// Get the client (workspace) name for a changelist
pub fn get_changelist_client(cl_number: &str) -> Result<Option<String>> {
    let output = Command::new("p4")
        .arg("change")
        .arg("-o")
        .arg(cl_number)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("Failed to execute p4 change -o {}", cl_number))?;
    
    if !output.status.success() {
        return Ok(None);
    }
    
    let stdout = String::from_utf8_lossy(&output.stdout);
    
    for line in stdout.lines() {
        if line.starts_with("Client:") {
            if let Some(client) = line.split_whitespace().nth(1) {
                return Ok(Some(client.to_string()));
            }
        }
    }
    
    Ok(None)
}

/// Get the current client (workspace) name
pub fn get_current_client() -> Result<String> {
    let output = Command::new("p4")
        .arg("client")
        .arg("-o")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("Failed to execute p4 client -o")?;
    
    if !output.status.success() {
        anyhow::bail!("Failed to get current client");
    }
    
    let stdout = String::from_utf8_lossy(&output.stdout);
    
    for line in stdout.lines() {
        if line.starts_with("Client:") {
            if let Some(client) = line.split_whitespace().nth(1) {
                return Ok(client.to_string());
            }
        }
    }
    
    anyhow::bail!("Could not determine current client")
}

/// Get the depot path for a local file using p4 where
pub fn get_depot_path(local_path: &str) -> Result<Option<String>> {
    // Try to canonicalize the path first (resolve relative paths, symlinks, etc.)
    let resolved_path = std::fs::canonicalize(local_path)
        .unwrap_or_else(|_| std::path::PathBuf::from(local_path));
    
    let path_str = resolved_path.to_string_lossy();
    
    let output = Command::new("p4")
        .arg("where")
        .arg(path_str.as_ref())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("Failed to run p4 where on {}", path_str))?;
    
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    
    // Check for common error messages
    if stderr.contains("not in client view") || stderr.contains("file(s) not in client view") {
        eprintln!("Debug: File is not in the Perforce client view");
        eprintln!("Debug: stderr: {}", stderr.trim());
        return Ok(None);
    }
    
    if !output.status.success() {
        eprintln!("Debug: p4 where failed");
        eprintln!("Debug: stderr: {}", stderr.trim());
        eprintln!("Debug: stdout: {}", stdout.trim());
        return Ok(None);
    }
    
    // p4 where output format: depot_path client_path local_path
    // We want the first field (depot path)
    if let Some(line) = stdout.lines().next() {
        if let Some(depot_path) = line.split_whitespace().next() {
            if depot_path.starts_with("//") {
                return Ok(Some(depot_path.to_string()));
            }
        }
    }
    
    eprintln!("Debug: Could not parse depot path from p4 where output");
    eprintln!("Debug: stdout: {}", stdout.trim());
    Ok(None)
}

/// Get the local path for a depot file using p4 where
pub fn get_local_path(depot_path: &str) -> Result<Option<String>> {
    let output = Command::new("p4")
        .arg("where")
        .arg(depot_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("Failed to run p4 where on {}", depot_path))?;
    
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    
    if stderr.contains("not in client view") || stderr.contains("file(s) not in client view") {
        return Ok(None);
    }
    
    if !output.status.success() {
        return Ok(None);
    }
    
    // Parse output: depot-path client-path local-path
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            // The third field is the local path
            return Ok(Some(parts[2].to_string()));
        }
    }
    
    Ok(None)
}

#[derive(Debug, Clone)]
pub struct AnnotateLine {
    pub cl_number: String,
    pub username: String,
    pub date: String,
    pub line_content: String,
}

/// Get annotate information for a file
pub fn get_annotate(file_path: &str) -> Result<Vec<AnnotateLine>> {
    // Use -a -u flags: -a shows changelist ranges, -u adds user and date
    // Use -c to show changelist numbers instead of revision numbers
    // Use -I to follow all integrations
    let stdout = run("p4", &["annotate", "-a", "-u", "-c", "-I", "-q", file_path])?;
    
    // Format with -a -u flags: <cl-range>: <user> <date> <line>
    // Important: Use single space after date to preserve indentation in line content
    let line_re = Regex::new(r"^(\d+(?:-\d+)?):\s+(\S+)\s+(\d{4}/\d{2}/\d{2}) (.*)$").unwrap();
    
    let mut lines = Vec::new();
    for line in stdout.lines() {
        if let Some(cap) = line_re.captures(line) {
            lines.push(AnnotateLine {
                cl_number: cap[1].to_string(),
                username: cap[2].to_string(),
                date: cap[3].to_string(),
                line_content: cap[4].to_string(),
            });
        } else {
            // If the line doesn't match, it might be a continuation or malformed
            lines.push(AnnotateLine {
                cl_number: "?".to_string(),
                username: "?".to_string(),
                date: "?".to_string(),
                line_content: line.to_string(),
            });
        }
    }
    
    Ok(lines)
}
