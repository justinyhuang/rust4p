mod perforce;

use anyhow::Result;
use clap::{Parser, Subcommand};
use itertools::Itertools;
use owo_colors::OwoColorize;
use std::collections::HashMap;
use std::io::Write;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent},
    execute,
    terminal::{self, ClearType},
};
use glob::glob;

/// p — tiny Perforce helper CLI
#[derive(Parser)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show opened files grouped by changelist, with colored boxes.
    Opened,
    /// Interactive changelist selector for editing with p4 change.
    Change,
    /// Interactive file selector to reopen files to a different changelist.
    Reopen,
    /// Interactive file selector to revert files.
    Revert,
    /// Unshelve files from a changelist.
    Unshelve,
    /// Shelve files in a changelist.
    Shelve,
    /// Diff files in a changelist.
    Diff,
    /// Open a file for edit in a specific changelist.
    #[command(name = "open")]
    Open {
        /// Path(s) to the file(s) to open (supports wildcards)
        files: Vec<String>,
    },
    /// Add a new file to a specific changelist.
    #[command(name = "add")]
    Add {
        /// Path(s) to the file(s) to add (supports wildcards)
        files: Vec<String>,
    },
    /// Initialize a git repository in the current directory.
    #[command(name = "ginit")]
    Ginit,
    /// Remove git repository but keep all files.
    #[command(name = "gdeinit")]
    Gdeinit,
    /// Manage tracked changelists.
    #[command(name = "ls")]
    Ls,
    /// Show annotated file with CL, user, date, and line content.
    #[command(name = "annotate")]
    Annotate {
        /// Path to the file to annotate
        file: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Opened => cmd_opened()?,
        Commands::Change => cmd_change()?,
        Commands::Reopen => cmd_reopen()?,
        Commands::Revert => cmd_revert()?,
        Commands::Unshelve => cmd_unshelve()?,
        Commands::Shelve => cmd_shelve()?,
        Commands::Diff => cmd_diff()?,
        Commands::Open { files } => cmd_open(&files)?,
        Commands::Add { files } => cmd_add(&files)?,
        Commands::Ginit => cmd_ginit()?,
        Commands::Gdeinit => cmd_gdeinit()?,
        Commands::Ls => cmd_ls()?,
        Commands::Annotate { file } => cmd_annotate(&file)?,
    }
    Ok(())
}

fn cmd_opened() -> Result<()> {
    let opened = perforce::get_opened_files()?;

    // Group by changelist
    let mut map: HashMap<String, Vec<_>> = HashMap::new();
    for f in opened {
        map.entry(f.changelist.clone()).or_default().push(f);
    }

    // Stable order: default first, then numeric ascending, then others
    let mut keys: Vec<String> = map.keys().cloned().collect();
    keys.sort_by(|a, b| {
        if a == "default" && b != "default" {
            std::cmp::Ordering::Less
        } else if b == "default" && a != "default" {
            std::cmp::Ordering::Greater
        } else {
            // try numeric
            match (a.parse::<i64>(), b.parse::<i64>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                _ => a.cmp(b),
            }
        }
    });

    // Palette of colors for boxes
    let palette: Vec<fn(&str) -> String> = vec![
        |s| s.blue().to_string(),
        |s| s.green().to_string(),
        |s| s.magenta().to_string(),
        |s| s.cyan().to_string(),
        |s| s.yellow().to_string(),
        |s| s.bright_blue().to_string(),
        |s| s.bright_green().to_string(),
        |s| s.bright_magenta().to_string(),
        |s| s.bright_cyan().to_string(),
        |s| s.bright_yellow().to_string(),
    ];

    // Fetch descriptions for each CL
    let mut cl_descriptions: HashMap<String, String> = HashMap::new();
    for key in &keys {
        if key != "default" {
            if let Ok(Some(desc)) = perforce::get_change_description(key) {
                let first_line = desc.lines().next().unwrap_or("").trim();
                cl_descriptions.insert(key.clone(), first_line.to_string());
            }
        }
    }
    
    // Check for file differences between opened and shelved
    let mut cl_has_diff: HashMap<String, bool> = HashMap::new();
    for key in &keys {
        if key != "default" {
            let files = map.get(key).unwrap();
            let opened_files: std::collections::HashSet<String> = files
                .iter()
                .map(|f| f.depot_file.clone())
                .collect();
            
            if let Ok(shelved_files) = perforce::get_shelved_files(key) {
                let shelved_paths: std::collections::HashSet<String> = shelved_files
                    .iter()
                    .map(|f| f.depot_file.clone())
                    .collect();
                
                if opened_files != shelved_paths {
                    cl_has_diff.insert(key.clone(), true);
                }
            }
        }
    }

    // Calculate max width across all boxes first
    let mut max_width = 0usize;
    for key in &keys {
        let files = map.get(key).unwrap();
        let has_diff = cl_has_diff.get(key).copied().unwrap_or(false);
        let title = if key == "default" {
            "CL default (pending)".to_string()
        } else if has_diff {
            format!("CL {} [files differ from shelved]", key)
        } else {
            format!("CL {key}")
        };
        let header = format!(" {} — {} file(s) ", title, files.len());
        let lines: Vec<String> = files.iter().map(render_opened_line).collect_vec();
        
        // Include description in width calculation
        let desc = cl_descriptions.get(key).map(|s| s.as_str()).unwrap_or("");
        let desc_width = if !desc.is_empty() {
            visual_width(&format!(" {}", desc))
        } else {
            0
        };
        
        let box_width = std::cmp::max(
            std::cmp::max(visual_width(&header), desc_width),
            lines
                .iter()
                .map(|s| visual_width(s))
                .max()
                .unwrap_or(0),
        ) + 4;
        max_width = std::cmp::max(max_width, box_width);
    }

    // Render each changelist in a colored ASCII box
    let mut palette_idx = 0usize;
    let num_keys = keys.len();
    for (idx, key) in keys.iter().enumerate() {
        let files = map.get(key).unwrap();
        let has_diff = cl_has_diff.get(key).copied().unwrap_or(false);
        
        // Default changelist is always bright red
        let color = if key == "default" {
            |s: &str| s.bright_red().to_string()
        } else {
            palette[palette_idx % palette.len()]
        };
        if key != "default" {
        palette_idx += 1;
        }

        let title = if key == "default" {
            "CL default (pending)".to_string()
        } else if has_diff {
            format!("CL {} {}", key, "[files differ from shelved]".bright_yellow())
        } else {
            format!("CL {key}")
        };
        let header = format!(" {} — {} file(s) ", title, files.len());
        let description = cl_descriptions.get(key).map(|s| s.as_str()).unwrap_or("");
        let is_last = idx == num_keys - 1;
        print_box(&header, description, &files.iter().map(render_opened_line).collect_vec(), color, max_width, idx > 0, is_last);
    }
    
    // Show hint about viewing file differences
    println!();
    println!("{}", "Tip: Use 'p ls' and press 's' on a CL to view file differences.".bright_black());

    Ok(())
}

fn cmd_change() -> Result<()> {
    let opened = perforce::get_opened_files()?;
    
    // Group by changelist to get unique CLs
    let mut cls: Vec<String> = opened
        .iter()
        .map(|f| f.changelist.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    
    // Sort: default first, then numeric
    cls.sort_by(|a, b| {
        if a == "default" && b != "default" {
            std::cmp::Ordering::Less
        } else if b == "default" && a != "default" {
            std::cmp::Ordering::Greater
        } else {
            match (a.parse::<i64>(), b.parse::<i64>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                _ => a.cmp(b),
            }
        }
    });
    
    // Add "Create new CL" option at the beginning
    let mut options = vec!["[Create new CL]".to_string()];
    options.extend(cls.clone());
    
    // Fetch descriptions for each CL
    let mut cl_descriptions: HashMap<String, String> = HashMap::new();
    cl_descriptions.insert("[Create new CL]".to_string(), "Create a new changelist".to_string());
    for cl in &cls {
        if cl != "default" {
            if let Ok(Some(desc)) = perforce::get_change_description(cl) {
                // Get first line of description
                let first_line = desc.lines().next().unwrap_or("").trim();
                cl_descriptions.insert(cl.clone(), first_line.to_string());
            }
        }
    }
    
    println!("Select a changelist to edit:");
    println!();
    
    // Run interactive selector
    let selected = interactive_select_with_desc(&options, &cl_descriptions)?;
    
    if let Some(selection) = selected {
        let cl = if selection == "[Create new CL]" {
            // Create a new changelist
            println!("\nCreating new changelist...");
            let new_cl = perforce::create_changelist()?;
            add_tracked_cl(&new_cl)?;
            println!("{}", format!("✓ Created CL {}", new_cl).bright_green());
            println!();
            new_cl
        } else {
            selection
        };
        
        // Execute p4 change <CL>
        let mut cmd = std::process::Command::new("p4");
        cmd.arg("change").arg(&cl);
        cmd.stdin(std::process::Stdio::inherit());
        cmd.stdout(std::process::Stdio::inherit());
        cmd.stderr(std::process::Stdio::inherit());
        
        let status = cmd.status()?;
        if !status.success() {
            anyhow::bail!("p4 change command failed");
        }
    }
    
    Ok(())
}

fn cmd_reopen() -> Result<()> {
    let mut opened = perforce::get_opened_files()?;
    
    if opened.is_empty() {
        println!("No open files found.");
        return Ok(());
    }
    
    // Sort files by changelist to group them together
    opened.sort_by(|a, b| {
        if a.changelist == "default" && b.changelist != "default" {
            std::cmp::Ordering::Less
        } else if b.changelist == "default" && a.changelist != "default" {
            std::cmp::Ordering::Greater
        } else {
            match (a.changelist.parse::<i64>(), b.changelist.parse::<i64>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                _ => a.changelist.cmp(&b.changelist),
            }
        }
    });
    
    // Get unique CLs for color mapping (in sorted order)
    let mut cls: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for file in &opened {
        if seen.insert(file.changelist.clone()) {
            cls.push(file.changelist.clone());
        }
    }
    
    // Color palette
    let palette: Vec<fn(&str) -> String> = vec![
        |s| s.blue().to_string(),
        |s| s.green().to_string(),
        |s| s.magenta().to_string(),
        |s| s.cyan().to_string(),
        |s| s.yellow().to_string(),
        |s| s.bright_blue().to_string(),
        |s| s.bright_green().to_string(),
        |s| s.bright_magenta().to_string(),
        |s| s.bright_cyan().to_string(),
        |s| s.bright_yellow().to_string(),
    ];
    
    let bright_red_fn: fn(&str) -> String = |s| s.bright_red().to_string();
    
    // Map CL to color - default is always bright red
    let cl_to_color: HashMap<String, fn(&str) -> String> = cls
        .iter()
        .enumerate()
        .map(|(idx, cl)| {
            if cl == "default" {
                (cl.clone(), bright_red_fn)
            } else {
                (cl.clone(), palette[idx % palette.len()])
            }
        })
        .collect();
    
    // Fetch descriptions for each CL
    let mut cl_descriptions: HashMap<String, String> = HashMap::new();
    for cl in &cls {
        if cl != "default" {
            if let Ok(Some(desc)) = perforce::get_change_description(cl) {
                let first_line = desc.lines().next().unwrap_or("").trim();
                cl_descriptions.insert(cl.clone(), first_line.to_string());
            }
        }
    }
    
    // Print newline to establish starting position
    println!();
    
    // Interactive file selector
    let selected_files = interactive_file_select(&opened, &cl_to_color, &cl_descriptions, false)?;
    
    if selected_files.is_empty() {
        println!("No files selected.");
        return Ok(());
    }
    
    // Get CLs from opened files
    let opened_cls: std::collections::HashSet<String> = opened
        .iter()
        .map(|f| f.changelist.clone())
        .collect();
    
    // Get tracked CLs from .pconfig
    let tracked_cls_vec = read_tracked_cls()?;
    let tracked_cls: std::collections::HashSet<String> = tracked_cls_vec.into_iter().collect();
    
    // Combine opened CLs and tracked CLs
    let all_cls: std::collections::HashSet<String> = opened_cls.union(&tracked_cls).cloned().collect();
    
    // Convert to Vec and sort
    let mut dest_cls: Vec<String> = all_cls.into_iter().collect();
    dest_cls.sort_by(|a, b| {
        if a == "default" && b != "default" {
            std::cmp::Ordering::Less
        } else if b == "default" && a != "default" {
            std::cmp::Ordering::Greater
        } else {
            match (a.parse::<i64>(), b.parse::<i64>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                _ => a.cmp(b),
            }
        }
    });
    
    // Always include "default" if not already present
    if !dest_cls.contains(&"default".to_string()) {
        dest_cls.insert(0, "default".to_string());
    }
    
    // Add "new CL" option at the end
    dest_cls.push("new".to_string());
    
    // Fetch descriptions for destination CLs
    let mut dest_descriptions: HashMap<String, String> = HashMap::new();
    for cl in &dest_cls {
        if cl != "default" && cl != "new" {
            if let Ok(Some(desc)) = perforce::get_change_description(cl) {
                let first_line = desc.lines().next().unwrap_or("").trim();
                dest_descriptions.insert(cl.clone(), first_line.to_string());
            }
        }
    }
    
    // Show CL selector
    println!("\nSelect destination changelist:");
    let dest_cl = interactive_select_with_desc(&dest_cls, &dest_descriptions)?;
    
    if let Some(cl) = dest_cl {
        let final_cl = if cl == "new" {
            // Handle new CL flow
            println!("\nEnter CL number (or press Enter to create new CL):");
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            let input = input.trim();
            
            if input.is_empty() {
                // Create new CL
                println!("Creating new changelist...");
                let new_cl = perforce::create_changelist()?;
                add_tracked_cl(&new_cl)?;
                println!("Created CL {}", new_cl);
                new_cl
            } else {
                // Check if CL exists
                match perforce::get_change_description(input)? {
                    Some(desc) => {
                        println!("\nCL {} exists:", input);
                        println!("Description: {}", desc);
                        println!("\nConfirm unshelving and adding files to this CL? (y/n):");
                        
                        let mut confirm = String::new();
                        std::io::stdin().read_line(&mut confirm)?;
                        
                        if confirm.trim().to_lowercase() != "y" {
                            println!("Cancelled.");
                            return Ok(());
                        }
                        
                        // Unshelve the CL
                        println!("Unshelving CL {}...", input);
                        if let Err(e) = perforce::unshelve_changelist(input) {
                            eprintln!("Warning: Could not unshelve: {}", e);
                            println!("Continuing to reopen files...");
                        } else {
                            add_tracked_cl(input)?;
                        }
                        
                        input.to_string()
                    }
                    None => {
                        println!("Error: CL {} does not exist", input);
                        return Ok(());
                    }
                }
            }
        } else {
            cl
        };
        
        // Execute p4 reopen for each selected file
        println!("\nReopening {} file(s) to {}...", selected_files.len(), 
            if final_cl == "default" { "default changelist".to_string() } else { format!("CL {}", final_cl) });
        
        for file in &selected_files {
            let mut cmd = std::process::Command::new("p4");
            if final_cl == "default" {
                cmd.arg("reopen").arg("-c").arg("default").arg(&file.depot_file);
            } else {
                cmd.arg("reopen").arg("-c").arg(&final_cl).arg(&file.depot_file);
            }
            
            let output = cmd.output()?;
            if !output.status.success() {
                eprintln!("Failed to reopen {}: {}", file.depot_file, 
                    String::from_utf8_lossy(&output.stderr));
            } else {
                println!("✓ {}", file.depot_file);
            }
        }
        
        println!("\nDone!");
    }
    
    Ok(())
}

fn cmd_revert() -> Result<()> {
    let mut opened = perforce::get_opened_files()?;
    
    if opened.is_empty() {
        println!("No open files found.");
        return Ok(());
    }
    
    // Sort files by changelist to group them together
    opened.sort_by(|a, b| {
        if a.changelist == "default" && b.changelist != "default" {
            std::cmp::Ordering::Less
        } else if b.changelist == "default" && a.changelist != "default" {
            std::cmp::Ordering::Greater
        } else {
            match (a.changelist.parse::<i64>(), b.changelist.parse::<i64>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                _ => a.changelist.cmp(&b.changelist),
            }
        }
    });
    
    // Get unique CLs for color mapping (in sorted order)
    let mut cls: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for file in &opened {
        if seen.insert(file.changelist.clone()) {
            cls.push(file.changelist.clone());
        }
    }
    
    // Color palette
    let palette: Vec<fn(&str) -> String> = vec![
        |s| s.blue().to_string(),
        |s| s.green().to_string(),
        |s| s.magenta().to_string(),
        |s| s.cyan().to_string(),
        |s| s.yellow().to_string(),
        |s| s.bright_blue().to_string(),
        |s| s.bright_green().to_string(),
        |s| s.bright_magenta().to_string(),
        |s| s.bright_cyan().to_string(),
        |s| s.bright_yellow().to_string(),
    ];
    
    let bright_red_fn: fn(&str) -> String = |s| s.bright_red().to_string();
    
    // Map CL to color - default is always bright red
    let cl_to_color: HashMap<String, fn(&str) -> String> = cls
        .iter()
        .enumerate()
        .map(|(idx, cl)| {
            if cl == "default" {
                (cl.clone(), bright_red_fn)
            } else {
                (cl.clone(), palette[idx % palette.len()])
            }
        })
        .collect();
    
    // Fetch descriptions for each CL
    let mut cl_descriptions: HashMap<String, String> = HashMap::new();
    for cl in &cls {
        if cl != "default" {
            if let Ok(Some(desc)) = perforce::get_change_description(cl) {
                let first_line = desc.lines().next().unwrap_or("").trim();
                cl_descriptions.insert(cl.clone(), first_line.to_string());
            }
        }
    }
    
    // Print newline to establish starting position
    println!();
    
    // Interactive file selector
    let selected_files = interactive_file_select(&opened, &cl_to_color, &cl_descriptions, false)?;
    
    if selected_files.is_empty() {
        println!("No files selected.");
        return Ok(());
    }
    
    // Confirmation prompt
    println!("\nYou are about to revert {} file(s):", selected_files.len());
    for file in &selected_files {
        println!("  - {}", file.depot_file);
    }
    println!("\nThis will discard all changes. Are you sure? (yes/no):");
    
    let mut confirm = String::new();
    std::io::stdin().read_line(&mut confirm)?;
    
    if confirm.trim().to_lowercase() != "yes" {
        println!("Cancelled.");
        return Ok(());
    }
    
    // Execute p4 revert for each selected file
    println!("\nReverting {} file(s)...", selected_files.len());
    
    for file in &selected_files {
        let mut cmd = std::process::Command::new("p4");
        cmd.arg("revert").arg(&file.depot_file);
        
        let output = cmd.output()?;
        if !output.status.success() {
            eprintln!("Failed to revert {}: {}", file.depot_file, 
                String::from_utf8_lossy(&output.stderr));
        } else {
            println!("✓ {}", file.depot_file);
        }
    }
    
    println!("\nDone!");
    
    Ok(())
}

fn cmd_diff() -> Result<()> {
    let opened = perforce::get_opened_files()?;
    
    // Group by changelist
    let mut map: HashMap<String, Vec<perforce::OpenedFile>> = HashMap::new();
    for f in opened {
        map.entry(f.changelist.clone()).or_default().push(f);
    }

    // Stable order: default first, then numeric ascending
    let mut keys: Vec<String> = map.keys().cloned().collect();
    keys.sort_by(|a, b| {
        if a == "default" && b != "default" {
            std::cmp::Ordering::Less
        } else if b == "default" && a != "default" {
            std::cmp::Ordering::Greater
        } else {
            match (a.parse::<i64>(), b.parse::<i64>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                _ => a.cmp(b),
            }
        }
    });

    // Fetch descriptions for each CL
    let mut descriptions: HashMap<String, String> = HashMap::new();
    for key in &keys {
        if key != "default" {
            if let Ok(Some(desc)) = perforce::get_change_description(key) {
                let first_line = desc.lines().next().unwrap_or("").trim();
                descriptions.insert(key.clone(), first_line.to_string());
            }
        }
    }

    if keys.is_empty() {
        println!("No opened files found.");
        return Ok(());
    }

    println!("Select a changelist to diff:");
    println!();
    let selected_cl = match interactive_select_with_desc(&keys, &descriptions)? {
        Some(cl) => cl,
        None => {
            println!("No changelist selected.");
            return Ok(());
        }
    };
    
    // Get files from selected CL
    let files = map.get(&selected_cl).unwrap();
    
    // Run p4 diff on each file
    for file in files {
        println!("\n{}", "=".repeat(80).bright_blue());
        println!("{} {}", "Diff:".bright_yellow(), file.depot_file);
        println!("{}", "=".repeat(80).bright_blue());
        
        let _status = std::process::Command::new("p4")
            .arg("diff")
            .arg(&file.depot_file)
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()?;
    }

    Ok(())
}

fn cmd_open(file_paths: &[String]) -> Result<()> {
    if file_paths.is_empty() {
        eprintln!("Error: No files specified");
        return Ok(());
    }
    
    // Collect all files (already expanded by shell, or use as-is)
    let mut files: Vec<String> = Vec::new();
    
    for file_path in file_paths {
        // Check if it contains glob characters
        if file_path.contains('*') || file_path.contains('?') || file_path.contains('[') {
            // Try to expand glob pattern
            let mut found_any = false;
            for entry in glob(file_path)? {
                match entry {
                    Ok(path) => {
                        if path.is_file() {
                            files.push(path.to_string_lossy().to_string());
                            found_any = true;
                        }
                    }
                    Err(e) => eprintln!("Error reading glob entry: {}", e),
                }
            }
            if !found_any {
                eprintln!("Warning: No files match pattern '{}'", file_path);
            }
        } else {
            // File path already provided (likely expanded by shell)
            if std::path::Path::new(file_path).is_file() {
                files.push(file_path.clone());
            } else {
                eprintln!("Warning: File '{}' does not exist or is not a file", file_path);
            }
        }
    }
    
    if files.is_empty() {
        eprintln!("Error: No valid files found");
        return Ok(());
    }
    
    println!("Found {} file(s):", files.len());
    for file in &files {
        println!("  {}", file);
    }
    println!();

    // Get all open changelists
    let opened = perforce::get_opened_files()?;
    
    // Get unique CLs
    let mut cls: Vec<String> = opened
        .iter()
        .map(|f| f.changelist.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    
    // Sort: default first, then numeric
    cls.sort_by(|a, b| {
        if a == "default" && b != "default" {
            std::cmp::Ordering::Less
        } else if b == "default" && a != "default" {
            std::cmp::Ordering::Greater
        } else {
            match (a.parse::<i64>(), b.parse::<i64>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                _ => a.cmp(b),
            }
        }
    });
    
    // Always include "default" if not already present
    if !cls.contains(&"default".to_string()) {
        cls.insert(0, "default".to_string());
    }
    
    // Add "[Create new CL]" option at the beginning
    cls.insert(0, "[Create new CL]".to_string());
    
    // Fetch descriptions for each CL
    let mut cl_descriptions: HashMap<String, String> = HashMap::new();
    cl_descriptions.insert("[Create new CL]".to_string(), "Create a new changelist".to_string());
    for cl in &cls {
        if cl != "default" && cl != "[Create new CL]" {
            if let Ok(Some(desc)) = perforce::get_change_description(cl) {
                let first_line = desc.lines().next().unwrap_or("").trim();
                cl_descriptions.insert(cl.clone(), first_line.to_string());
            }
        }
    }
    
    println!("Select a changelist to open the file(s) to:");
    println!();
    
    let selected = match interactive_select_with_desc(&cls, &cl_descriptions)? {
        Some(cl) => cl,
        None => {
            println!("No changelist selected.");
            return Ok(());
        }
    };
    
    let selected_cl = if selected == "[Create new CL]" {
        // Create a new changelist
        println!("\nCreating new changelist...");
        let new_cl = perforce::create_changelist()?;
        add_tracked_cl(&new_cl)?;
        println!("{}", format!("✓ Created CL {}", new_cl).bright_green());
        println!();
        new_cl
    } else {
        selected
    };
    
    // Open all matching files
    let mut success_count = 0;
    let mut error_count = 0;
    
    println!("\nOpening files...");
    for file in &files {
        let output = std::process::Command::new("p4")
            .arg("edit")
            .arg("-c")
            .arg(&selected_cl)
            .arg(file)
            .output()?;
        
        if output.status.success() {
            println!("{} {}", "✓".bright_green(), file);
            success_count += 1;
        } else {
            println!("{} {}: {}", "✗".bright_red(), file, String::from_utf8_lossy(&output.stderr).trim());
            error_count += 1;
        }
    }
    
    println!();
    if success_count > 0 {
        println!("{}", format!("✓ {} file(s) opened successfully", success_count).bright_green());
    }
    if error_count > 0 {
        eprintln!("{}", format!("✗ {} file(s) failed to open", error_count).bright_red());
    }

    Ok(())
}

fn cmd_add(file_paths: &[String]) -> Result<()> {
    if file_paths.is_empty() {
        eprintln!("Error: No files specified");
        return Ok(());
    }
    
    // Collect all files (already expanded by shell, or use as-is)
    let mut files: Vec<String> = Vec::new();
    
    for file_path in file_paths {
        // Check if it contains glob characters
        if file_path.contains('*') || file_path.contains('?') || file_path.contains('[') {
            // Try to expand glob pattern
            let mut found_any = false;
            for entry in glob(file_path)? {
                match entry {
                    Ok(path) => {
                        if path.is_file() {
                            files.push(path.to_string_lossy().to_string());
                            found_any = true;
                        }
                    }
                    Err(e) => eprintln!("Error reading glob entry: {}", e),
                }
            }
            if !found_any {
                eprintln!("Warning: No files match pattern '{}'", file_path);
            }
        } else {
            // File path already provided (likely expanded by shell)
            if std::path::Path::new(file_path).is_file() {
                files.push(file_path.clone());
            } else {
                eprintln!("Warning: File '{}' does not exist or is not a file", file_path);
            }
        }
    }
    
    if files.is_empty() {
        eprintln!("Error: No valid files found");
        return Ok(());
    }
    
    println!("Found {} file(s):", files.len());
    for file in &files {
        println!("  {}", file);
    }
    println!();

    // Get all open changelists
    let opened = perforce::get_opened_files()?;
    
    // Get unique CLs
    let mut cls: Vec<String> = opened
        .iter()
        .map(|f| f.changelist.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    
    // Sort: default first, then numeric
    cls.sort_by(|a, b| {
        if a == "default" && b != "default" {
            std::cmp::Ordering::Less
        } else if b == "default" && a != "default" {
            std::cmp::Ordering::Greater
        } else {
            match (a.parse::<i64>(), b.parse::<i64>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                _ => a.cmp(b),
            }
        }
    });
    
    // Always include "default" if not already present
    if !cls.contains(&"default".to_string()) {
        cls.insert(0, "default".to_string());
    }
    
    // Add "[Create new CL]" option at the beginning
    cls.insert(0, "[Create new CL]".to_string());
    
    // Fetch descriptions for each CL
    let mut cl_descriptions: HashMap<String, String> = HashMap::new();
    cl_descriptions.insert("[Create new CL]".to_string(), "Create a new changelist".to_string());
    for cl in &cls {
        if cl != "default" && cl != "[Create new CL]" {
            if let Ok(Some(desc)) = perforce::get_change_description(cl) {
                let first_line = desc.lines().next().unwrap_or("").trim();
                cl_descriptions.insert(cl.clone(), first_line.to_string());
            }
        }
    }
    
    println!("Select a changelist to add the file(s) to:");
    println!();
    
    let selected = match interactive_select_with_desc(&cls, &cl_descriptions)? {
        Some(cl) => cl,
        None => {
            println!("No changelist selected.");
            return Ok(());
        }
    };
    
    let selected_cl = if selected == "[Create new CL]" {
        // Create a new changelist
        println!("\nCreating new changelist...");
        let new_cl = perforce::create_changelist()?;
        add_tracked_cl(&new_cl)?;
        println!("{}", format!("✓ Created CL {}", new_cl).bright_green());
        println!();
        new_cl
    } else {
        selected
    };
    
    // Add all matching files
    let mut success_count = 0;
    let mut error_count = 0;
    
    println!("\nAdding files...");
    for file in &files {
        let output = std::process::Command::new("p4")
            .arg("add")
            .arg("-c")
            .arg(&selected_cl)
            .arg(file)
            .output()?;
        
        if output.status.success() {
            println!("{} {}", "✓".bright_green(), file);
            success_count += 1;
        } else {
            println!("{} {}: {}", "✗".bright_red(), file, String::from_utf8_lossy(&output.stderr).trim());
            error_count += 1;
        }
    }
    
    println!();
    if success_count > 0 {
        println!("{}", format!("✓ {} file(s) added successfully", success_count).bright_green());
    }
    if error_count > 0 {
        eprintln!("{}", format!("✗ {} file(s) failed to add", error_count).bright_red());
    }

    Ok(())
}

fn cmd_unshelve() -> Result<()> {
    // Get tracked CLs
    let tracked_cls = read_tracked_cls()?;
    
    // Get currently opened files
    let opened = perforce::get_opened_files()?;
    
    // Build a set of CLs with opened files
    let cls_with_files: std::collections::HashSet<String> = opened
        .iter()
        .map(|f| f.changelist.clone())
        .filter(|cl| cl != "default")
        .collect();
    
    // Filter tracked CLs to those without opened files
    let empty_cls: Vec<String> = tracked_cls
        .into_iter()
        .filter(|cl| !cls_with_files.contains(cl))
        .collect();
    
    // Build options list
    let mut options: Vec<String> = empty_cls.clone();
    options.push("[Enter CL number manually]".to_string());
    
    if empty_cls.is_empty() {
        println!("{}", "No tracked CLs without opened files.".bright_yellow());
        println!("You can still enter a CL number manually.");
        println!();
    } else {
        println!("Select a CL to unshelve (tracked CLs without opened files):");
        println!();
    }
    
    // Fetch descriptions
    let mut cl_descriptions: HashMap<String, String> = HashMap::new();
    for cl in &empty_cls {
        if let Ok(Some(desc)) = perforce::get_change_description(cl) {
            let first_line = desc.lines().next().unwrap_or("").trim();
            cl_descriptions.insert(cl.clone(), first_line.to_string());
        }
    }
    
    // Show interactive selector
    let selection = if !options.is_empty() {
        interactive_select_with_desc(&options, &cl_descriptions)?
    } else {
        None
    };
    
    let cl_number = match selection {
        None => {
            println!("Cancelled.");
            return Ok(());
        }
        Some(s) if s == "[Enter CL number manually]" => {
            // Manual entry
            println!("\nEnter CL number to unshelve:");
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            let cl = input.trim();
            
            if cl.is_empty() {
                println!("Error: No CL number provided");
                return Ok(());
            }
            
            // Validate it's a number
            if cl.parse::<i64>().is_err() {
                println!("Error: Invalid CL number '{}'", cl);
                return Ok(());
            }
            
            // Check if CL exists
            match perforce::get_change_description(cl)? {
                None => {
                    println!("Error: CL {} does not exist", cl);
                    return Ok(());
                }
                Some(desc) => {
                    println!("\nCL {} found:", cl);
                    let first_line = desc.lines().next().unwrap_or("(no description)");
                    println!("Description: {}", first_line);
                }
            }
            
            cl.to_string()
        }
        Some(cl) => cl,
    };
    
    // Check if CL belongs to a different client
    let source_cl = cl_number.clone();
    let mut dest_cl = source_cl.clone();
    
    let current_client = perforce::get_current_client()?;
    let cl_client = perforce::get_changelist_client(&source_cl)?;
    
    if let Some(ref cl_client_name) = cl_client {
        if cl_client_name != &current_client {
            println!("{}", format!("\nWarning: CL {} belongs to a different client: {}", 
                source_cl, cl_client_name).bright_yellow());
            println!("{}", format!("Your current client: {}", current_client).bright_cyan());
            println!("\nDo you want to unshelve to a different CL? (y/n)");
            
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            let response = input.trim().to_lowercase();
            
            if response == "y" || response == "yes" {
                // Get all CLs for selection
                let opened = perforce::get_opened_files()?;
                let mut map: HashMap<String, Vec<perforce::OpenedFile>> = HashMap::new();
                for f in opened {
                    map.entry(f.changelist.clone()).or_default().push(f);
                }
                
                let mut all_cls: Vec<String> = map.keys().cloned().collect();
                all_cls.sort();
                
                // Add "default" if not already in the list
                if !all_cls.contains(&"default".to_string()) {
                    all_cls.insert(0, "default".to_string());
                }
                
                // Add "create new CL" option at the beginning
                all_cls.insert(0, "[Create new CL]".to_string());
                
                // Fetch descriptions
                let mut cl_descriptions: HashMap<String, String> = HashMap::new();
                for cl in &all_cls {
                    if cl == "[Create new CL]" {
                        continue;
                    }
                    if let Ok(Some(desc)) = perforce::get_change_description(cl) {
                        let first_line = desc.lines().next().unwrap_or("").trim();
                        cl_descriptions.insert(cl.clone(), first_line.to_string());
                    }
                }
                
                println!("\nSelect destination CL:");
                println!();
                
                match interactive_select_with_desc(&all_cls, &cl_descriptions)? {
                    None => {
                        println!("Cancelled.");
                        return Ok(());
                    }
                    Some(s) if s == "[Create new CL]" => {
                        let new_cl = perforce::create_changelist()?;
                        println!("Created new CL: {}", new_cl);
                        dest_cl = new_cl;
                    }
                    Some(cl) => {
                        dest_cl = cl;
                    }
                }
            }
        }
    }
    
    // Get shelved files
    let shelved_files = perforce::get_shelved_files(&source_cl)?;
    
    if shelved_files.is_empty() {
        println!("No shelved files found in CL {}", source_cl);
        return Ok(());
    }
    
    // Show files for selection
    println!("\nSelect files to unshelve from CL {}:", source_cl);
    println!();
    
    // Create a simple color map (all files are from same CL)
    let palette: Vec<fn(&str) -> String> = vec![|s| s.blue().to_string()];
    let mut cl_to_color: HashMap<String, fn(&str) -> String> = HashMap::new();
    cl_to_color.insert(source_cl.clone(), palette[0]);
    
    let cl_descriptions: HashMap<String, String> = HashMap::new();
    
    // Interactive file selector - all files pre-selected
    let selected_files = interactive_file_select(&shelved_files, &cl_to_color, &cl_descriptions, true)?;
    
    if selected_files.is_empty() {
        println!("No files selected.");
        return Ok(());
    }
    
    // Collect depot paths
    let file_paths: Vec<String> = selected_files.iter().map(|f| f.depot_file.clone()).collect();
    
    // Check if we can actually use the source CL (i.e., it belongs to current client)
    let can_use_source_cl = cl_client.as_ref().map(|c| c == &current_client).unwrap_or(true);
    
    // Get files currently in default BEFORE unshelving
    let opened_before = perforce::get_opened_files()?;
    let default_files_before: std::collections::HashSet<String> = opened_before
        .iter()
        .filter(|f| f.changelist == "default")
        .map(|f| f.depot_file.clone())
        .collect();
    
    // Unshelve the selected files
    if source_cl == dest_cl {
        println!("\nUnshelving {} file(s) from CL {}...", file_paths.len(), source_cl);
        match perforce::unshelve_files(&source_cl, &file_paths) {
            Ok(_) => {
                println!("✓ Successfully unshelved {} file(s) from CL {}", file_paths.len(), source_cl);
            }
            Err(e) => {
                eprintln!("Error unshelving: {}", e);
                return Err(e);
            }
        }
        
        // Only try to reopen files if the CL belongs to current client
        if can_use_source_cl {
            // Reopen files to the same CL
            println!("\nReopening unshelved files to CL {}...", source_cl);
            
            // Get files currently in default AFTER unshelving
            let opened_after = perforce::get_opened_files()?;
            let default_files_after: Vec<_> = opened_after
                .iter()
                .filter(|f| f.changelist == "default")
                .filter(|f| !default_files_before.contains(&f.depot_file)) // Only new files
                .collect();
            
            if default_files_after.is_empty() {
                println!("No newly unshelved files found in default changelist to reopen");
                return Ok(());
            }
            
            println!("Reopening {} file(s) to CL {}...", default_files_after.len(), source_cl);
            
            for file in default_files_after {
                let mut cmd = std::process::Command::new("p4");
                cmd.arg("reopen").arg("-c").arg(&source_cl).arg(&file.depot_file);
                
                let output = cmd.output()?;
                if !output.status.success() {
                    eprintln!("Warning: Failed to reopen {}: {}", 
                        file.depot_file, 
                        String::from_utf8_lossy(&output.stderr));
                } else {
                    println!("✓ {}", file.depot_file);
                }
            }
            
            add_tracked_cl(&source_cl)?;
            println!("\nDone! CL {} is ready for use.", source_cl);
        } else {
            // CL belongs to different client, files stay in default
            println!("\nFiles have been unshelved to the default changelist.");
            println!("Use 'p reopen' to move them to a changelist if needed.");
        }
    } else {
        // Unshelve to a different CL - need to use -c flag
        println!("\nUnshelving {} file(s) from CL {} to CL {}...", file_paths.len(), source_cl, dest_cl);
        
        let mut cmd = std::process::Command::new("p4");
        cmd.arg("unshelve")
            .arg("-s")
            .arg(&source_cl)
            .arg("-c")
            .arg(&dest_cl);
        
        for file in &file_paths {
            cmd.arg(file);
        }
        
        let output = cmd.output()?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            eprintln!("Error unshelving: {}", err);
            return Err(anyhow::anyhow!("Failed to unshelve: {}", err));
        }
        
        add_tracked_cl(&source_cl)?;
        add_tracked_cl(&dest_cl)?;
        println!("✓ Successfully unshelved {} file(s) from CL {} to CL {}", file_paths.len(), source_cl, dest_cl);
        println!("\nDone! CL {} is ready for use.", dest_cl);
    }
    
    Ok(())
}

fn cmd_shelve() -> Result<()> {
    let opened = perforce::get_opened_files()?;
    
    // Group by changelist
    let mut map: HashMap<String, Vec<perforce::OpenedFile>> = HashMap::new();
    for f in opened {
        map.entry(f.changelist.clone()).or_default().push(f);
    }

    // Stable order: default first, then numeric ascending
    let mut keys: Vec<String> = map.keys().cloned().collect();
    keys.sort_by(|a, b| {
        if a == "default" && b != "default" {
            std::cmp::Ordering::Less
        } else if b == "default" && a != "default" {
            std::cmp::Ordering::Greater
        } else {
            match (a.parse::<i64>(), b.parse::<i64>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                _ => a.cmp(b),
            }
        }
    });

    // Fetch descriptions for each CL
    let mut descriptions: HashMap<String, String> = HashMap::new();
    for key in &keys {
        if key != "default" {
            if let Ok(Some(desc)) = perforce::get_change_description(key) {
                let first_line = desc.lines().next().unwrap_or("").trim();
                descriptions.insert(key.clone(), first_line.to_string());
            }
        }
    }

    if keys.is_empty() {
        println!("No opened files found.");
        return Ok(());
    }

    println!("Select a changelist to shelve:");
        println!();
    let selected_cl = match interactive_select_with_desc(&keys, &descriptions)? {
        Some(cl) => cl,
        None => {
            println!("No changelist selected.");
            return Ok(());
        }
    };
    
    // Get files from selected CL
    let files = map.get(&selected_cl).unwrap();
    
    println!("\nShelving {} file(s) from CL {}...", files.len(), selected_cl);
    
    // Run p4 shelve -r -c <CL>
    // The -r flag replaces all shelved files, removing files no longer in the CL
    let output = std::process::Command::new("p4")
        .arg("shelve")
        .arg("-r")
        .arg("-c")
        .arg(&selected_cl)
        .output()?;
    
    if output.status.success() {
        add_tracked_cl(&selected_cl)?;
        println!("\n{}", "✓ Successfully shelved files!".bright_green());
        print!("{}", String::from_utf8_lossy(&output.stdout));
    } else {
        eprintln!("\n{}", "Error shelving files:".bright_red());
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        return Err(anyhow::anyhow!("p4 shelve command failed"));
    }

    Ok(())
}

fn cmd_ginit() -> Result<()> {
    // Get current directory
    let current_dir = std::env::current_dir()?;
    let current_path = current_dir.display();
    
    // Check if .git directory already exists
    let git_dir = current_dir.join(".git");
    if git_dir.exists() {
        println!("{}", "A git repository already exists in this directory.".bright_yellow());
        println!("Path: {}", current_path);
        return Ok(());
    }
    
    // Ask for confirmation
    println!("Initialize a git repository in: {}", current_path.to_string().bright_cyan());
    println!("Are you sure? (y/n):");
    
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let answer = input.trim().to_lowercase();
    
    if answer != "y" && answer != "yes" {
        println!("Cancelled.");
        return Ok(());
    }
    
    // Run git init
    let output = std::process::Command::new("git")
        .arg("init")
        .output()?;
    
    if !output.status.success() {
        eprintln!("\n{}", "Error initializing git repository:".bright_red());
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        return Ok(());
    }
    
    println!("\n{}", "✓ Git repository initialized successfully!".bright_green());
    print!("{}", String::from_utf8_lossy(&output.stdout));
    
    // Get opened Perforce files
    println!("\nChecking for Perforce opened files...");
    let opened_files = match perforce::get_opened_files() {
        Ok(files) => files,
        Err(_) => {
            println!("No Perforce files opened or not in a Perforce workspace.");
            return Ok(());
        }
    };
    
    if opened_files.is_empty() {
        println!("No Perforce files are currently opened.");
        return Ok(());
    }
    
    // Filter files that are under the current directory and collect their info
    let current_dir_str = current_dir.to_string_lossy();
    let mut files_info: Vec<(String, String, Option<String>)> = Vec::new(); // (local_path, depot_path, workrev)
    
    for file in &opened_files {
        // Get the local file path by running p4 where on the depot path
        let output = std::process::Command::new("p4")
            .arg("where")
            .arg(&file.depot_file)
            .output()?;
        
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // p4 where output: depot_path client_path local_path
            if let Some(line) = stdout.lines().next() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 3 {
                    let local_path = parts[2];
                    // Check if the local path is under current directory
                    if local_path.starts_with(current_dir_str.as_ref()) {
                        files_info.push((
                            local_path.to_string(),
                            file.depot_file.clone(),
                            file.workrev.clone()
                        ));
                    }
                }
            }
        }
    }
    
    if files_info.is_empty() {
        println!("No Perforce files found under the current directory.");
        return Ok(());
    }
    
    // Step 1: Save current working versions (with P4 changes)
    println!("\n{}", "Step 1: Saving current file versions...".bright_cyan());
    let mut saved_contents: Vec<(String, Vec<u8>)> = Vec::new();
    for (local_path, _, _) in &files_info {
        if let Ok(content) = std::fs::read(local_path) {
            saved_contents.push((local_path.clone(), content));
            println!("  {} {}", "✓".bright_green(), local_path);
        }
    }
    
    // Step 2: Restore original versions from Perforce
    println!("\n{}", "Step 2: Restoring original file versions from Perforce...".bright_cyan());
    for (local_path, depot_path, workrev) in &files_info {
        // Construct the depot path with revision
        let depot_with_rev = if let Some(rev) = workrev {
            format!("{}#{}", depot_path, rev)
        } else {
            format!("{}#have", depot_path)
        };
        
        // Get the original content using p4 print
        let output = std::process::Command::new("p4")
            .arg("print")
            .arg("-q") // quiet, no extra output
            .arg(&depot_with_rev)
            .output()?;
        
        if output.status.success() {
            std::fs::write(local_path, &output.stdout)?;
            println!("  {} {}", "✓".bright_green(), local_path);
        } else {
            eprintln!("  {} {} - {}", "✗".bright_red(), local_path,
                String::from_utf8_lossy(&output.stderr).trim());
        }
    }
    
    // Step 3: Stage original versions and create initial commit
    println!("\n{}", "Step 3: Creating initial commit with original versions...".bright_cyan());
    for (local_path, _, _) in &files_info {
        std::process::Command::new("git")
            .arg("add")
            .arg(local_path)
            .output()?;
    }
    
    let commit_output = std::process::Command::new("git")
        .arg("commit")
        .arg("-m")
        .arg("Initial commit: Original versions from Perforce")
        .output()?;
    
    if commit_output.status.success() {
        println!("{}", "✓ Initial commit created".bright_green());
    } else {
        eprintln!("{} {}", "✗".bright_red(), 
            String::from_utf8_lossy(&commit_output.stderr).trim());
    }
    
    // Step 4: Restore current working versions (with changes)
    println!("\n{}", "Step 4: Restoring your current changes...".bright_cyan());
    for (local_path, content) in &saved_contents {
        std::fs::write(local_path, content)?;
        println!("  {} {}", "✓".bright_green(), local_path);
    }
    
    // Show git status
    println!("\n{}", "Git status:".bright_cyan());
    let status_output = std::process::Command::new("git")
        .arg("status")
        .arg("--short")
        .output()?;
    
    if status_output.status.success() {
        print!("{}", String::from_utf8_lossy(&status_output.stdout));
    }
    
    println!("\n{}", format!("✓ Complete! {} file(s) ready with your changes", files_info.len()).bright_green());
    println!("{}", "  Initial commit contains the original Perforce versions".bright_blue());
    println!("{}", "  Your changes are unstaged - use 'git diff' to see them".bright_blue());
    
    Ok(())
}

fn cmd_gdeinit() -> Result<()> {
    // Get current directory
    let current_dir = std::env::current_dir()?;
    let current_path = current_dir.display();
    
    // Check if .git directory exists
    let git_dir = current_dir.join(".git");
    if !git_dir.exists() {
        println!("{}", "No git repository found in this directory.".bright_yellow());
        println!("Path: {}", current_path);
        return Ok(());
    }
    
    // Ask for confirmation
    println!("{}", "⚠️  WARNING: This will remove the git repository!".bright_red().bold());
    println!("Directory: {}", current_path.to_string().bright_cyan());
    println!("All files will be kept, only the .git directory will be removed.");
    println!("\nAre you sure? (y/n):");
    
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let answer = input.trim().to_lowercase();
    
    if answer != "y" && answer != "yes" {
        println!("Cancelled.");
        return Ok(());
    }
    
    // Remove .git directory
    match std::fs::remove_dir_all(&git_dir) {
        Ok(_) => {
            println!("\n{}", "✓ Git repository removed successfully!".bright_green());
            println!("All files have been preserved.");
        }
        Err(e) => {
            eprintln!("\n{}", "Error removing git repository:".bright_red());
            eprintln!("{}", e);
            return Err(e.into());
        }
    }

    Ok(())
}

fn cmd_ls() -> Result<()> {
    loop {
        // Get tracked CLs from config
        let tracked_cls = read_tracked_cls()?;
        
        // Get currently opened files
        let opened = perforce::get_opened_files()?;
        
        // Build a map of CL -> file count
        let mut cl_file_count: HashMap<String, usize> = HashMap::new();
        for file in &opened {
            *cl_file_count.entry(file.changelist.clone()).or_insert(0) += 1;
        }
        
        // Combine tracked CLs with currently opened CLs
        let mut all_cls: std::collections::HashSet<String> = tracked_cls.iter().cloned().collect();
        for cl in cl_file_count.keys() {
            if cl != "default" {
                all_cls.insert(cl.clone());
            }
        }
        
        let mut cls: Vec<String> = all_cls.into_iter().collect();
        
        // Sort: numeric ascending
        cls.sort_by(|a, b| {
            match (a.parse::<i64>(), b.parse::<i64>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                _ => a.cmp(b),
            }
        });
        
        if cls.is_empty() {
            println!("{}", "No tracked changelists found.".bright_yellow());
            println!("Create, shelve, or unshelve changelists to track them.");
            return Ok(());
        }
        
        // Fetch descriptions for each CL
        let mut cl_descriptions: HashMap<String, String> = HashMap::new();
        for cl in &cls {
            if let Ok(Some(desc)) = perforce::get_change_description(cl) {
                let first_line = desc.lines().next().unwrap_or("").trim();
                cl_descriptions.insert(cl.clone(), first_line.to_string());
            }
        }
        
        // Check if opened files differ from shelved files for each CL
        let mut cl_has_diff: HashMap<String, bool> = HashMap::new();
        for cl in &cls {
            let file_count = cl_file_count.get(cl).copied().unwrap_or(0);
            
            // Only check if CL has opened files
            if file_count > 0 {
                // Get opened files for this CL
                let opened_files: std::collections::HashSet<String> = opened
                    .iter()
                    .filter(|f| &f.changelist == cl)
                    .map(|f| f.depot_file.clone())
                    .collect();
                
                // Get shelved files for this CL
                if let Ok(shelved_files) = perforce::get_shelved_files(cl) {
                    let shelved_paths: std::collections::HashSet<String> = shelved_files
                        .iter()
                        .map(|f| f.depot_file.clone())
                        .collect();
                    
                    // Check if the sets differ
                    if opened_files != shelved_paths {
                        cl_has_diff.insert(cl.clone(), true);
                    }
                }
            }
        }
        
        println!("Tracked changelists:");
        println!();
        
        // Use interactive selector with delete capability
        match interactive_cl_select_with_delete(&cls, &cl_descriptions, &cl_file_count, &cl_has_diff)? {
            None => {
                // User cancelled or quit
                return Ok(());
            }
            Some(_) => {
                // A CL was deleted, loop to refresh the list
                println!();
                continue;
            }
        }
    }
}

// Enum to represent items in the selection list
#[derive(Clone)]
enum SelectItem {
    ClHeader(String), // changelist number/name
    File(usize),      // index into the files array
}

fn interactive_file_select(
    files: &[perforce::OpenedFile],
    cl_to_color: &HashMap<String, fn(&str) -> String>,
    cl_descriptions: &HashMap<String, String>,
    pre_select_all: bool,
) -> Result<Vec<perforce::OpenedFile>> {
    // Group files by changelist
    let mut cl_to_files: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, file) in files.iter().enumerate() {
        cl_to_files.entry(file.changelist.clone()).or_default().push(idx);
    }
    
    // Sort CLs: default first, then numeric
    let mut cls: Vec<String> = cl_to_files.keys().cloned().collect();
    cls.sort_by(|a, b| {
        if a == "default" && b != "default" {
            std::cmp::Ordering::Less
        } else if b == "default" && a != "default" {
            std::cmp::Ordering::Greater
        } else {
            match (a.parse::<i64>(), b.parse::<i64>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                _ => a.cmp(b),
            }
        }
    });
    
    // Build the display list: CL headers + files
    let mut items: Vec<SelectItem> = Vec::new();
    for cl in &cls {
        items.push(SelectItem::ClHeader(cl.clone()));
        for &file_idx in &cl_to_files[cl] {
            items.push(SelectItem::File(file_idx));
        }
    }
    
    let mut selected_idx = 0usize;
    let mut selected_set: std::collections::HashSet<usize> = std::collections::HashSet::new();
    
    // Pre-select all files if requested
    if pre_select_all {
        for i in 0..files.len() {
            selected_set.insert(i);
        }
    }
    
    // Capture the starting position (before entering raw mode)
    let start_pos = cursor::position()?;
    
    // Enable raw mode
    terminal::enable_raw_mode()?;
    
    let result = (|| -> Result<Vec<perforce::OpenedFile>> {
        // Track the actual rendering position (may differ from start_pos after first render)
        let mut render_pos = start_pos;
        let mut first_render = true;
        
        loop {
            // Move cursor to render position and clear from here down
            execute!(
                std::io::stdout(),
                cursor::MoveTo(render_pos.0, render_pos.1),
                terminal::Clear(ClearType::FromCursorDown)
            )?;
            std::io::stdout().flush()?;
            
            // Display header
            print!("Select files or CLs (↑/↓ to navigate, Tab to jump to next CL, Space to toggle, Enter to confirm, Esc/q to cancel):\r\n\r\n");
            
            // Display items
            for (idx, item) in items.iter().enumerate() {
                let arrow = if idx == selected_idx { "→" } else { " " };
                
                match item {
                    SelectItem::ClHeader(cl) => {
                        let color = cl_to_color.get(cl).unwrap();
                        let cl_label = if cl == "default" {
                            "default (pending)".to_string()
                        } else {
                            cl.clone()
                        };
                        
                        // Check if all files in this CL are selected
                        let file_indices = &cl_to_files[cl];
                        let all_selected = file_indices.iter().all(|&i| selected_set.contains(&i));
                        let some_selected = file_indices.iter().any(|&i| selected_set.contains(&i));
                        
                        let checkbox = if all_selected {
                            "[✓]"
                        } else if some_selected {
                            "[◐]"
                        } else {
                            "[ ]"
                        };
                        
                        // Format with description if available
                        let line = if let Some(desc) = cl_descriptions.get(cl) {
                            format!("{}  {} 📋 CL {} - {} — {} file(s)", 
                                arrow, checkbox, cl_label, desc, file_indices.len())
                        } else {
                            format!("{}  {} 📋 CL {} — {} file(s)", 
                                arrow, checkbox, cl_label, file_indices.len())
                        };
                        
                        if idx == selected_idx {
                            print!("{}\r\n", color(&line).bold().to_string());
                        } else {
                            print!("{}\r\n", color(&line).bold().to_string());
                        }
                    }
                    SelectItem::File(file_idx) => {
                        let file = &files[*file_idx];
                        let color = cl_to_color.get(&file.changelist).unwrap();
                        
                        let checkbox = if selected_set.contains(file_idx) { "[✓]" } else { "[ ]" };
                        
                        let line = format!("  {}  {}     {}", 
                            arrow, checkbox, file.depot_file);
                        
                        if idx == selected_idx {
                            print!("{}\r\n", color(&line).bold().to_string());
                        } else {
                            print!("{}\r\n", color(&line));
                        }
                    }
                }
            }
            
            print!("\r\n");
            print!("Selected: {} file(s)\r\n", selected_set.len());
            
            std::io::stdout().flush()?;
            
            // After first render, adjust render_pos if scrolling occurred
            if first_render {
                let end_pos = cursor::position()?;
                let lines_rendered = 2 + items.len() + 2; // header + blank + items + blank + footer
                
                // Calculate where we should have ended up (cursor is after last line)
                let expected_end_row = render_pos.1 + lines_rendered as u16;
                
                // If actual position is different, terminal scrolled
                if end_pos.1 != expected_end_row {
                    // Recalculate render_pos based on where we actually ended
                    if end_pos.1 >= lines_rendered as u16 {
                        render_pos.1 = end_pos.1 - lines_rendered as u16;
                    } else {
                        render_pos.1 = 0;
                    }
                }
                first_render = false;
            }
            
            // Read key event
            if let Event::Key(KeyEvent { code, .. }) = event::read()? {
                match code {
                    KeyCode::Up => {
                        if selected_idx > 0 {
                            selected_idx -= 1;
                        } else {
                            // Wrap to bottom
                            selected_idx = items.len() - 1;
                        }
                    }
                    KeyCode::Down => {
                        if selected_idx < items.len() - 1 {
                            selected_idx += 1;
                        } else {
                            // Wrap to top
                            selected_idx = 0;
                        }
                    }
                    KeyCode::Tab => {
                        // Jump to the next CL header
                        let mut found_next = false;
                        for i in (selected_idx + 1)..items.len() {
                            if matches!(items[i], SelectItem::ClHeader(_)) {
                                selected_idx = i;
                                found_next = true;
                                break;
                            }
                        }
                        // If no CL found after current position, wrap to first CL
                        if !found_next {
                            for i in 0..=selected_idx {
                                if matches!(items[i], SelectItem::ClHeader(_)) {
                                    selected_idx = i;
                                    break;
                                }
                            }
                        }
                    }
                    KeyCode::BackTab => {
                        // Jump to the previous CL header (Shift+Tab)
                        let mut found_prev = false;
                        for i in (0..selected_idx).rev() {
                            if matches!(items[i], SelectItem::ClHeader(_)) {
                                selected_idx = i;
                                found_prev = true;
                                break;
                            }
                        }
                        // If no CL found before current position, wrap to last CL
                        if !found_prev {
                            for i in (selected_idx..items.len()).rev() {
                                if matches!(items[i], SelectItem::ClHeader(_)) {
                                    selected_idx = i;
                                    break;
                                }
                            }
                        }
                    }
                    KeyCode::Char(' ') => {
                        match &items[selected_idx] {
                            SelectItem::ClHeader(cl) => {
                                // Toggle all files in this CL
                                let file_indices = &cl_to_files[cl];
                                let all_selected = file_indices.iter().all(|&i| selected_set.contains(&i));
                                
                                if all_selected {
                                    // Deselect all
                                    for &file_idx in file_indices {
                                        selected_set.remove(&file_idx);
                                    }
                                } else {
                                    // Select all
                                    for &file_idx in file_indices {
                                        selected_set.insert(file_idx);
                                    }
                                }
                            }
                            SelectItem::File(file_idx) => {
                                // Toggle single file
                                if selected_set.contains(file_idx) {
                                    selected_set.remove(file_idx);
                                } else {
                                    selected_set.insert(*file_idx);
                                }
                            }
                        }
                    }
                    KeyCode::Enter => {
                        terminal::disable_raw_mode()?;
                        // Clear the menu
                        execute!(
                            std::io::stdout(),
                            cursor::MoveTo(render_pos.0, render_pos.1),
                            terminal::Clear(ClearType::FromCursorDown)
                        )?;
                        
                        let mut result = Vec::new();
                        for idx in selected_set {
                            result.push(files[idx].clone());
                        }
                        return Ok(result);
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        terminal::disable_raw_mode()?;
                        // Clear the menu
                        execute!(
                            std::io::stdout(),
                            cursor::MoveTo(render_pos.0, render_pos.1),
                            terminal::Clear(ClearType::FromCursorDown)
                        )?;
                        println!("Cancelled.");
                        return Ok(Vec::new());
                    }
                    _ => {}
                }
            }
        }
    })();
    
    // Always disable raw mode on exit
    terminal::disable_raw_mode()?;
    
    result
}

fn interactive_cl_select_with_delete(
    items: &[String],
    descriptions: &HashMap<String, String>,
    file_counts: &HashMap<String, usize>,
    has_diff: &HashMap<String, bool>,
) -> Result<Option<String>> {
    let mut selected_idx = 0usize;
    
    // Capture the starting position (before entering raw mode)
    let start_pos = cursor::position()?;
    
    // Enable raw mode
    terminal::enable_raw_mode()?;
    
    let result = (|| -> Result<Option<String>> {
        // Track the actual rendering position (may differ from start_pos after first render)
        let mut render_pos = start_pos;
        let mut first_render = true;
        
        loop {
            // Move cursor to render position and clear from here down
            execute!(
                std::io::stdout(),
                cursor::MoveTo(render_pos.0, render_pos.1),
                terminal::Clear(ClearType::FromCursorDown)
            )?;
            std::io::stdout().flush()?;
            
            // Display header
            print!("Tracked CLs (↑/↓ to navigate, 'd' to delete, 'u' to unshelve, 's' to show file diff, Esc/q to cancel):\r\n\r\n");
            
            // Display items
            for (idx, item) in items.iter().enumerate() {
                let file_count = file_counts.get(item).copied().unwrap_or(0);
                let desc = descriptions.get(item).map(|s| s.as_str()).unwrap_or("");
                let has_file_diff = has_diff.get(item).copied().unwrap_or(false);
                
                let display = if file_count == 0 {
                    // Empty CL - show in gray
                    if desc.is_empty() {
                        format!("CL {} [empty]", item).bright_black().to_string()
                    } else {
                        format!("CL {} - {} [empty]", item, desc).bright_black().to_string()
                    }
                } else {
                    // CL with files
                    let base_text = if desc.is_empty() {
                        format!("CL {} — {} file(s)", item, file_count)
                    } else {
                        format!("CL {} - {} — {} file(s)", item, desc, file_count)
                    };
                    
                    // If files differ from shelved, add indicator with only the indicator in yellow
                    if has_file_diff {
                        format!("{} {}", base_text, "[files differ from shelved]".bright_yellow())
                    } else {
                        base_text
                    }
                };
                
                if idx == selected_idx {
                    print!("  {}  {}\r\n", "→".bright_green(), display.bright_green().bold());
                } else {
                    print!("     {}\r\n", display);
                }
            }
            
            std::io::stdout().flush()?;
            
            // After first render, adjust render_pos if scrolling occurred
            if first_render {
                let end_pos = cursor::position()?;
                let lines_rendered = 2 + items.len(); // header + blank + items
                
                // Calculate where we should have ended up (cursor is after last line)
                let expected_end_row = render_pos.1 + lines_rendered as u16;
                
                // If actual position is different, terminal scrolled
                if end_pos.1 != expected_end_row {
                    // Recalculate render_pos based on where we actually ended
                    if end_pos.1 >= lines_rendered as u16 {
                        render_pos.1 = end_pos.1 - lines_rendered as u16;
                    } else {
                        render_pos.1 = 0;
                    }
                }
                first_render = false;
            }
            
            // Read key event
            if let Event::Key(KeyEvent { code, .. }) = event::read()? {
                match code {
                    KeyCode::Up => {
                        if selected_idx > 0 {
                            selected_idx -= 1;
                        } else {
                            selected_idx = items.len() - 1;
                        }
                    }
                    KeyCode::Down => {
                        if selected_idx < items.len() - 1 {
                            selected_idx += 1;
                        } else {
                            selected_idx = 0;
                        }
                    }
                    KeyCode::Char('d') | KeyCode::Char('D') => {
                        let cl = &items[selected_idx];
                        terminal::disable_raw_mode()?;
                        
                        // Clear the menu
                        execute!(
                            std::io::stdout(),
                            cursor::MoveTo(render_pos.0, render_pos.1),
                            terminal::Clear(ClearType::FromCursorDown)
                        )?;
                        
                        // Ask for confirmation
                        println!("{}", format!("Delete CL {}?", cl).bright_yellow().bold());
                        if let Some(desc) = descriptions.get(cl) {
                            println!("Description: {}", desc.bright_cyan());
                        }
                        
                        let file_count = file_counts.get(cl).copied().unwrap_or(0);
                        if file_count > 0 {
                            println!("{}", format!("This will revert {} opened file(s).", file_count).bright_red());
                        }
                        
                        println!("\nType 'yes' to confirm deletion:");
                        
                        let mut input = String::new();
                        std::io::stdin().read_line(&mut input)?;
                        let answer = input.trim().to_lowercase();
                        
                        if answer == "yes" {
                            // Revert all opened files in this CL
                            if file_count > 0 {
                                println!("\nReverting files...");
                                let opened = perforce::get_opened_files()?;
                                let files_in_cl: Vec<_> = opened.iter()
                                    .filter(|f| &f.changelist == cl)
                                    .collect();
                                
                                for file in files_in_cl {
                                    println!("  Reverting: {}", file.depot_file);
                                    let output = std::process::Command::new("p4")
                                        .arg("revert")
                                        .arg(&file.depot_file)
                                        .output()?;
                                    
                                    if !output.status.success() {
                                        eprintln!("    {}", "Error:".bright_red());
                                        eprintln!("    {}", String::from_utf8_lossy(&output.stderr));
                                    }
                                }
                            }
                            
                            // Remove from tracked CLs
                            remove_tracked_cl(cl)?;
                            
                            println!("\n{}", format!("✓ CL {} deleted and removed from tracking.", cl).bright_green());
                            return Ok(Some(cl.clone()));
                        } else {
                            println!("Deletion cancelled.");
                            // Re-enable raw mode and continue
                            terminal::enable_raw_mode()?;
                        }
                    }
                    KeyCode::Char('u') | KeyCode::Char('U') => {
                        let cl = items[selected_idx].clone();
                        terminal::disable_raw_mode()?;
                        
                        // Clear the menu
                        execute!(
                            std::io::stdout(),
                            cursor::MoveTo(render_pos.0, render_pos.1),
                            terminal::Clear(ClearType::FromCursorDown)
                        )?;
                        
                        println!("Selected: CL {}", cl.bright_cyan().bold());
                        if let Some(desc) = descriptions.get(&cl) {
                            println!("Description: {}", desc.bright_cyan());
                        }
                        println!();
                        
                        // Get shelved files
                        let shelved_files = perforce::get_shelved_files(&cl)?;
                        
                        if shelved_files.is_empty() {
                            println!("No shelved files found in CL {}", cl);
                            println!("\nPress any key to continue...");
                            terminal::enable_raw_mode()?;
                            event::read()?;
                            continue;
                        }
                        
                        // Check if CL is from a different client
                        let cl_client = perforce::get_changelist_client(&cl)?;
                        let current_client = perforce::get_current_client()?;
                        
                        let dest_cl = if cl_client.as_ref() != Some(&current_client) {
                            let cl_client_name = cl_client.as_deref().unwrap_or("unknown");
                            println!("{}", format!("Warning: CL {} belongs to client '{}', but you're in client '{}'.", 
                                cl, cl_client_name, current_client).bright_yellow());
                            println!("\nDo you want to unshelve to a different CL? (y/N):");
                            
                            let mut input = String::new();
                            std::io::stdin().read_line(&mut input)?;
                            let answer = input.trim().to_lowercase();
                            
                            if answer == "y" || answer == "yes" {
                                // Get CLs without opened files
                                let opened = perforce::get_opened_files()?;
                                let mut cl_file_count: HashMap<String, usize> = HashMap::new();
                                for file in &opened {
                                    *cl_file_count.entry(file.changelist.clone()).or_insert(0) += 1;
                                }
                                
                                let tracked_cls = read_tracked_cls()?;
                                let mut empty_cls: Vec<_> = tracked_cls.iter()
                                    .filter(|c| cl_file_count.get(*c).copied().unwrap_or(0) == 0)
                                    .cloned()
                                    .collect();
                                
                                empty_cls.sort_by(|a, b| {
                                    match (a.parse::<i64>(), b.parse::<i64>()) {
                                        (Ok(x), Ok(y)) => x.cmp(&y),
                                        _ => a.cmp(b),
                                    }
                                });
                                
                                let mut cl_descriptions: HashMap<String, String> = HashMap::new();
                                for c in &empty_cls {
                                    if let Ok(Some(desc)) = perforce::get_change_description(c) {
                                        let first_line = desc.lines().next().unwrap_or("").trim();
                                        cl_descriptions.insert(c.clone(), first_line.to_string());
                                    }
                                }
                                
                                empty_cls.push("[Create new CL]".to_string());
                                cl_descriptions.insert("[Create new CL]".to_string(), "Create a new changelist".to_string());
                                
                                println!("\nSelect destination CL:");
                                println!();
                                
                                if let Some(target) = interactive_select_with_desc(&empty_cls, &cl_descriptions)? {
                                    if target == "[Create new CL]" {
                                        let new_cl = perforce::create_changelist()?;
                                        println!("Created new CL: {}", new_cl.bright_green());
                                        new_cl
                                    } else {
                                        target
                                    }
                                } else {
                                    println!("Cancelled.");
                                    terminal::enable_raw_mode()?;
                                    continue;
                                }
                            } else {
                                cl.clone()
                            }
                        } else {
                            cl.clone()
                        };
                        
                        println!("\nSelect files to unshelve from CL {}:", cl);
                        println!();
                        
                        // Create a simple color map
                        let palette: Vec<fn(&str) -> String> = vec![|s| s.blue().to_string()];
                        let mut cl_to_color: HashMap<String, fn(&str) -> String> = HashMap::new();
                        cl_to_color.insert(cl.clone(), palette[0]);
                        
                        let cl_descriptions_empty: HashMap<String, String> = HashMap::new();
                        
                        // Interactive file selector - all files pre-selected
                        let selected_files = interactive_file_select(&shelved_files, &cl_to_color, &cl_descriptions_empty, true)?;
                        
                        if selected_files.is_empty() {
                            println!("No files selected.");
                            terminal::enable_raw_mode()?;
                            continue;
                        }
                        
                        // Collect depot paths
                        let file_paths: Vec<String> = selected_files.iter().map(|f| f.depot_file.clone()).collect();
                        
                        // Unshelve the selected files
                        if cl == dest_cl {
                            println!("\nUnshelving {} file(s) from CL {}...", file_paths.len(), cl);
                            match perforce::unshelve_files(&cl, &file_paths) {
                                Ok(_) => {
                                    add_tracked_cl(&cl)?;
                                    println!("✓ Successfully unshelved {} file(s) from CL {}", file_paths.len(), cl);
                                }
                                Err(e) => {
                                    eprintln!("Error unshelving: {}", e);
                                    println!("\nPress any key to continue...");
                                    terminal::enable_raw_mode()?;
                                    event::read()?;
                                    continue;
                                }
                            }
                            
                            // Reopen files to the original CL
                            if cl != "default" {
                                println!("\nReopening files to CL {}...", cl);
                                
                                // Get opened files to find files in default CL that need reopening
                                let opened = perforce::get_opened_files()?;
                                let default_files: Vec<_> = opened
                                    .iter()
                                    .filter(|f| f.changelist == "default" && file_paths.contains(&f.depot_file))
                                    .collect();
                                
                                for file in default_files {
                                    let mut cmd = std::process::Command::new("p4");
                                    cmd.arg("reopen").arg("-c").arg(&cl).arg(&file.depot_file);
                                    
                                    let output = cmd.output()?;
                                    if !output.status.success() {
                                        eprintln!("Warning: Failed to reopen {}: {}", 
                                            file.depot_file, 
                                            String::from_utf8_lossy(&output.stderr));
                                    } else {
                                        println!("  ✓ {}", file.depot_file);
                                    }
                                }
                            }
                        } else {
                            println!("\nUnshelving {} file(s) from CL {} to CL {}...", file_paths.len(), cl, dest_cl);
                            
                            let mut cmd = std::process::Command::new("p4");
                            cmd.arg("unshelve")
                                .arg("-s")
                                .arg(&cl)
                                .arg("-c")
                                .arg(&dest_cl);
                            
                            for file in &file_paths {
                                cmd.arg(file);
                            }
                            
                            let output = cmd.output()?;
                            
                            if output.status.success() {
                                add_tracked_cl(&dest_cl)?;
                                println!("✓ Successfully unshelved {} file(s) from CL {} to CL {}", 
                                    file_paths.len(), cl, dest_cl);
                            } else {
                                eprintln!("Error unshelving:");
                                eprintln!("{}", String::from_utf8_lossy(&output.stderr));
                                println!("\nPress any key to continue...");
                                terminal::enable_raw_mode()?;
                                event::read()?;
                                continue;
                            }
                        }
                        
                        println!("\nPress any key to continue...");
                        terminal::enable_raw_mode()?;
                        event::read()?;
                    }
                    KeyCode::Char('s') | KeyCode::Char('S') => {
                        let cl = items[selected_idx].clone();
                        terminal::disable_raw_mode()?;
                        
                        // Clear the menu
                        execute!(
                            std::io::stdout(),
                            cursor::MoveTo(render_pos.0, render_pos.1),
                            terminal::Clear(ClearType::FromCursorDown)
                        )?;
                        
                        println!("File diff for CL {}", cl.bright_cyan().bold());
                        if let Some(desc) = descriptions.get(&cl) {
                            println!("Description: {}", desc.bright_cyan());
                        }
                        println!();
                        
                        let file_count = file_counts.get(&cl).copied().unwrap_or(0);
                        
                        if file_count == 0 {
                            println!("No opened files in CL {}", cl);
                            println!("\nPress 'q' to return...");
                            terminal::enable_raw_mode()?;
                            loop {
                                if let Event::Key(KeyEvent { code, .. }) = event::read()? {
                                    if matches!(code, KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc) {
                                        break;
                                    }
                                }
                            }
                            continue;
                        }
                        
                        // Get opened files for this CL
                        let opened = perforce::get_opened_files()?;
                        let opened_files: Vec<_> = opened
                            .iter()
                            .filter(|f| &f.changelist == &cl)
                            .map(|f| f.depot_file.clone())
                            .collect();
                        
                        // Get shelved files for this CL
                        let shelved_result = perforce::get_shelved_files(&cl);
                        
                        match shelved_result {
                            Ok(shelved_files) => {
                                let shelved_paths: Vec<_> = shelved_files
                                    .iter()
                                    .map(|f| f.depot_file.clone())
                                    .collect();
                                
                                let opened_set: std::collections::HashSet<_> = opened_files.iter().collect();
                                let shelved_set: std::collections::HashSet<_> = shelved_paths.iter().collect();
                                
                                // Files only in opened (not shelved)
                                let only_opened: Vec<_> = opened_set.difference(&shelved_set).collect();
                                
                                // Files only in shelved (not opened)
                                let only_shelved: Vec<_> = shelved_set.difference(&opened_set).collect();
                                
                                if only_opened.is_empty() && only_shelved.is_empty() {
                                    println!("{}", "No differences - opened files match shelved files.".bright_green());
                                } else {
                                    if !only_opened.is_empty() {
                                        println!("{}", "Files opened locally but not shelved:".bright_yellow().bold());
                                        for file in &only_opened {
                                            println!("  {} {}", "+".bright_green(), file);
                                        }
                                        println!();
                                    }
                                    
                                    if !only_shelved.is_empty() {
                                        println!("{}", "Files shelved but not opened locally:".bright_yellow().bold());
                                        for file in &only_shelved {
                                            println!("  {} {}", "-".bright_red(), file);
                                        }
                                        println!();
                                    }
                                }
                            }
                            Err(_) => {
                                println!("{}", "No shelved files found in this CL.".bright_yellow());
                                println!();
                                println!("Opened files:");
                                for file in &opened_files {
                                    println!("  {}", file);
                                }
                                println!();
                            }
                        }
                        
                        println!("\nPress 'q' to return...");
                        terminal::enable_raw_mode()?;
                        loop {
                            if let Event::Key(KeyEvent { code, .. }) = event::read()? {
                                if matches!(code, KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc) {
                                    break;
                                }
                            }
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        terminal::disable_raw_mode()?;
                        execute!(
                            std::io::stdout(),
                            cursor::MoveTo(render_pos.0, render_pos.1),
                            terminal::Clear(ClearType::FromCursorDown)
                        )?;
                        println!("Cancelled.");
                        return Ok(None);
                    }
                    _ => {}
                }
            }
        }
    })();
    
    // Always disable raw mode on exit
    terminal::disable_raw_mode()?;
    
    result
}

fn interactive_select_with_desc(items: &[String], descriptions: &HashMap<String, String>) -> Result<Option<String>> {
    let mut selected_idx = 0usize;
    
    // Capture the starting position (before entering raw mode)
    let start_pos = cursor::position()?;
    
    // Enable raw mode
    terminal::enable_raw_mode()?;
    
    let result = (|| -> Result<Option<String>> {
        // Track the actual rendering position (may differ from start_pos after first render)
        let mut render_pos = start_pos;
        let mut first_render = true;
        
        loop {
            // Move cursor to render position and clear from here down
            execute!(
                std::io::stdout(),
                cursor::MoveTo(render_pos.0, render_pos.1),
                terminal::Clear(ClearType::FromCursorDown)
            )?;
            std::io::stdout().flush()?;
            
            // Display header
            print!("Select a changelist (↑/↓ to navigate, Enter to edit, Esc/q to cancel):\r\n\r\n");
            
            // Display items
            for (idx, item) in items.iter().enumerate() {
                let display = if item == "default" {
                    "CL default (pending)".to_string()
                } else if item == "new" {
                    "→ new CL".to_string()
                } else {
                    let desc = descriptions.get(item).map(|s| s.as_str()).unwrap_or("");
                    if desc.is_empty() {
                        format!("CL {}", item)
                    } else {
                        format!("CL {} - {}", item, desc)
                    }
                };
                
                if idx == selected_idx {
                    print!("  {}  {}\r\n", "→".bright_green(), display.bright_green().bold());
                } else {
                    print!("     {}\r\n", display);
                }
            }
            
            std::io::stdout().flush()?;
            
            // After first render, adjust render_pos if scrolling occurred
            if first_render {
                let end_pos = cursor::position()?;
                let lines_rendered = 2 + items.len(); // header + blank + items
                
                // Calculate where we should have ended up (cursor is after last line)
                let expected_end_row = render_pos.1 + lines_rendered as u16;
                
                // If actual position is different, terminal scrolled
                if end_pos.1 != expected_end_row {
                    // Recalculate render_pos based on where we actually ended
                    if end_pos.1 >= lines_rendered as u16 {
                        render_pos.1 = end_pos.1 - lines_rendered as u16;
                    } else {
                        render_pos.1 = 0;
                    }
                }
                first_render = false;
            }
            
            // Read key event
            if let Event::Key(KeyEvent { code, .. }) = event::read()? {
                match code {
                    KeyCode::Up => {
                        if selected_idx > 0 {
                            selected_idx -= 1;
                        } else {
                            // Wrap to bottom
                            selected_idx = items.len() - 1;
                        }
                    }
                    KeyCode::Down => {
                        if selected_idx < items.len() - 1 {
                            selected_idx += 1;
                        } else {
                            // Wrap to top
                            selected_idx = 0;
                        }
                    }
                    KeyCode::Enter => {
                        let result = items[selected_idx].clone();
                        terminal::disable_raw_mode()?;
                        // Clear the menu and print final selection
                        execute!(
                            std::io::stdout(),
                            cursor::MoveTo(render_pos.0, render_pos.1),
                            terminal::Clear(ClearType::FromCursorDown)
                        )?;
                        println!("Selected: {}", if result == "default" {
                            "CL default (pending)".to_string()
                        } else {
                            format!("CL {}", result)
                        });
                        return Ok(Some(result));
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        terminal::disable_raw_mode()?;
                        // Clear the menu
                        execute!(
                            std::io::stdout(),
                            cursor::MoveTo(render_pos.0, render_pos.1),
                            terminal::Clear(ClearType::FromCursorDown)
                        )?;
                        println!("Cancelled.");
                        return Ok(None);
                    }
                    _ => {}
                }
            }
        }
    })();
    
    // Always disable raw mode on exit
    terminal::disable_raw_mode()?;
    
    result
}

fn action_emoji(action: &str) -> &str {
    match action {
        "edit" => "✏️",
        "add" => "➕",
        "delete" => "🗑️",
        "integrate" => "🔀",
        "branch" => "🌿",
        "move/add" => "📦",
        "move/delete" => "📤",
        _ => "📄",
    }
}

fn render_opened_line(f: &perforce::OpenedFile) -> String {
    let rev = f.workrev.as_deref().unwrap_or("-");
    let emoji = action_emoji(&f.action);
    // Manually format to ensure proper alignment despite emoji width variations
    format!("{} {:<10} {:<6} {:<4} {}",
        emoji, f.action, "rev", rev, f.depot_file)
}

fn visual_width(s: &str) -> usize {
    // Strip ANSI escape codes for accurate width calculation
    let ansi_re = regex::Regex::new(r"\x1b\[[0-9;]*m").unwrap();
    let stripped = ansi_re.replace_all(s, "");
    unicode_width::UnicodeWidthStr::width(stripped.as_ref())
}

fn print_box<F>(title: &str, description: &str, lines: &[String], colorize: F, width: usize, skip_top: bool, is_last: bool)
where
    F: Fn(&str) -> String + Copy,
{
    let (tl, tr, bl, br, h, v, ml, mr) = ("┌", "┐", "└", "┘", "─", "│", "├", "┤");

    let top = format!("{tl}{}{tr}", h.repeat(width));
    let mid = format!("{ml}{}{mr}", h.repeat(width));
    let bot = format!("{bl}{}{br}", h.repeat(width));

    if skip_top {
        println!("{}", colorize(&mid));
    } else {
    println!("{}", colorize(&top));
    }
    // Make title bold
    let bold_title = format!("\x1b[1m{}\x1b[0m", title);
    let title_visual_width = visual_width(&bold_title);
    let title_pad = width - 2 - title_visual_width;
    println!(
        "{}",
        colorize(&format!("{v} {}{:pad$} {v}", bold_title, "", pad = title_pad))
    );
    
    // Print description if provided
    if !description.is_empty() {
        let desc_pad = width - 2 - visual_width(description);
    println!(
        "{}",
            colorize(&format!("{v} {}{:pad$} {v}", description, "", pad = desc_pad))
    );
    }
    
    for l in lines {
        let pad = width - 2 - visual_width(l);
        println!("{}", colorize(&format!("{v} {}{:pad$} {v}", l, "", pad = pad)));
    }
    if is_last {
    println!("{}", colorize(&bot));
    }
}

// ============================================================================
// Config file management for tracking CLs
// ============================================================================

fn get_config_path() -> Result<std::path::PathBuf> {
    let home = std::env::var("HOME")?;
    Ok(std::path::PathBuf::from(home).join(".pconfig"))
}

fn read_tracked_cls() -> Result<Vec<String>> {
    let config_path = get_config_path()?;
    if !config_path.exists() {
        return Ok(Vec::new());
    }
    
    let content = std::fs::read_to_string(config_path)?;
    let cls: Vec<String> = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
        .collect();
    
    Ok(cls)
}

fn write_tracked_cls(cls: &[String]) -> Result<()> {
    let config_path = get_config_path()?;
    let content = cls.join("\n");
    std::fs::write(config_path, content)?;
    Ok(())
}

fn add_tracked_cl(cl: &str) -> Result<()> {
    let mut cls = read_tracked_cls()?;
    if !cls.contains(&cl.to_string()) {
        cls.push(cl.to_string());
        write_tracked_cls(&cls)?;
    }
    Ok(())
}

fn remove_tracked_cl(cl: &str) -> Result<()> {
    let mut cls = read_tracked_cls()?;
    cls.retain(|c| c != cl);
    write_tracked_cls(&cls)?;
    Ok(())
}

fn cmd_annotate(file_path: &str) -> Result<()> {
    // Show loading indicator
    print!("Loading annotate data");
    std::io::stdout().flush()?;
    
    // Start spinner in a separate thread
    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let running_clone = running.clone();
    
    let spinner_thread = std::thread::spawn(move || {
        let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let mut idx = 0;
        while running_clone.load(std::sync::atomic::Ordering::Relaxed) {
            print!("\rLoading annotate data {}  ", frames[idx]);
            std::io::stdout().flush().ok();
            idx = (idx + 1) % frames.len();
            std::thread::sleep(std::time::Duration::from_millis(80));
        }
        print!("\r{}\r", " ".repeat(50)); // Clear the line
        std::io::stdout().flush().ok();
    });
    
    // Fetch the data
    let lines = perforce::get_annotate(file_path)?;
    
    // Stop spinner
    running.store(false, std::sync::atomic::Ordering::Relaxed);
    spinner_thread.join().ok();
    
    if lines.is_empty() {
        println!("No annotate data for file: {}", file_path);
        return Ok(());
    }
    
    // Enter raw mode for interactive viewing
    terminal::enable_raw_mode()?;
    
    let mut stdout = std::io::stdout();
    execute!(stdout, terminal::Clear(ClearType::All), cursor::Hide)?;
    
    let result = annotate_viewer(&lines);
    
    // Clean up terminal state
    execute!(stdout, cursor::Show)?;
    terminal::disable_raw_mode()?;
    
    result
}

fn annotate_viewer(lines: &[perforce::AnnotateLine]) -> Result<()> {
    let mut top_line = 0;
    let mut search_query: Option<String> = None;
    let mut search_matches: Vec<usize> = Vec::new();
    let mut current_match_idx: Option<usize> = None;
    
    loop {
        let (_, term_height) = terminal::size()?;
        let visible_lines = (term_height as usize).saturating_sub(2); // Leave space for status bar
        
        // Render the visible portion
        render_annotate_page(lines, top_line, visible_lines, &search_query, &search_matches, current_match_idx)?;
        
        // Handle keyboard input
        if let Event::Key(KeyEvent { code, .. }) = event::read()? {
            match code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::PageDown | KeyCode::Char(' ') => {
                    top_line = (top_line + visible_lines).min(lines.len().saturating_sub(1));
                }
                KeyCode::PageUp => {
                    top_line = top_line.saturating_sub(visible_lines);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    top_line = (top_line + 1).min(lines.len().saturating_sub(visible_lines));
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    top_line = top_line.saturating_sub(1);
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    top_line = 0;
                }
                KeyCode::End | KeyCode::Char('G') => {
                    top_line = lines.len().saturating_sub(visible_lines);
                }
                KeyCode::Char('/') => {
                    // Enter search mode
                    if let Some(query) = prompt_search()? {
                        search_query = Some(query.to_lowercase());
                        search_matches = find_search_matches(lines, &search_query.as_ref().unwrap());
                        current_match_idx = if !search_matches.is_empty() {
                            Some(0)
                        } else {
                            None
                        };
                        // Jump to first match
                        if let Some(0) = current_match_idx {
                            if !search_matches.is_empty() {
                                top_line = search_matches[0].saturating_sub(visible_lines / 2);
                            }
                        }
                    }
                }
                KeyCode::Char('n') => {
                    // Next match
                    if let Some(idx) = current_match_idx {
                        if !search_matches.is_empty() {
                            let next_idx = (idx + 1) % search_matches.len();
                            current_match_idx = Some(next_idx);
                            top_line = search_matches[next_idx].saturating_sub(visible_lines / 2);
                        }
                    }
                }
                KeyCode::Char('p') | KeyCode::Char('N') => {
                    // Previous match
                    if let Some(idx) = current_match_idx {
                        if !search_matches.is_empty() {
                            let prev_idx = if idx == 0 { search_matches.len() - 1 } else { idx - 1 };
                            current_match_idx = Some(prev_idx);
                            top_line = search_matches[prev_idx].saturating_sub(visible_lines / 2);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    
    Ok(())
}

fn render_annotate_page(
    lines: &[perforce::AnnotateLine],
    top_line: usize,
    visible_lines: usize,
    search_query: &Option<String>,
    search_matches: &[usize],
    current_match_idx: Option<usize>,
) -> Result<()> {
    let mut stdout = std::io::stdout();
    let (term_width, _) = terminal::size()?;
    
    execute!(stdout, cursor::MoveTo(0, 0))?;
    
    let end_line = (top_line + visible_lines).min(lines.len());
    
    // Find the max width for each column to align properly
    let max_cl_width = lines.iter()
        .map(|l| l.cl_number.len())
        .max()
        .unwrap_or(8)
        .max(8);
    let max_user_width = lines.iter()
        .map(|l| l.username.len())
        .max()
        .unwrap_or(10)
        .max(10);
    
    for i in top_line..end_line {
        let line = &lines[i];
        
        // Clear the entire line first
        execute!(stdout, terminal::Clear(ClearType::CurrentLine))?;
        
        // Check if this line is a search match
        let is_current_match = current_match_idx
            .and_then(|idx| search_matches.get(idx))
            .map(|&match_line| match_line == i)
            .unwrap_or(false);
        
        let is_match = search_matches.contains(&i);
        
        // Format the line with proper column alignment
        let formatted = format!(
            "{:>width_cl$} {:width_user$} {} {}",
            line.cl_number,
            line.username,
            line.date,
            line.line_content,
            width_cl = max_cl_width,
            width_user = max_user_width,
        );
        
        // Truncate to terminal width if necessary to prevent wrapping
        // Use char-based truncation to handle Unicode properly
        let truncated = if formatted.chars().count() > term_width as usize {
            let mut truncated_str = formatted.chars()
                .take(term_width as usize - 1)
                .collect::<String>();
            truncated_str.push('…');
            truncated_str
        } else {
            formatted
        };
        
        // Highlight current match or regular match
        if is_current_match {
            write!(stdout, "{}\r\n", truncated.black().on_yellow())?;
        } else if is_match {
            write!(stdout, "{}\r\n", truncated.on_bright_black())?;
        } else {
            write!(stdout, "{}\r\n", truncated)?;
        }
    }
    
    // Clear remaining lines
    for _ in end_line..top_line + visible_lines {
        execute!(stdout, terminal::Clear(ClearType::CurrentLine))?;
        write!(stdout, "\r\n")?;
    }
    
    // Status bar
    execute!(stdout, cursor::MoveTo(0, visible_lines as u16), terminal::Clear(ClearType::CurrentLine))?;
    let status = if let Some(ref query) = search_query {
        if let Some(idx) = current_match_idx {
            format!(
                "Lines {}-{}/{} | Search: '{}' ({}/{} matches) | q:quit /:search n:next p:prev",
                top_line + 1,
                end_line,
                lines.len(),
                query,
                idx + 1,
                search_matches.len()
            )
        } else {
            format!(
                "Lines {}-{}/{} | Search: '{}' (no matches) | q:quit /:search",
                top_line + 1,
                end_line,
                lines.len(),
                query
            )
        }
    } else {
        format!(
            "Lines {}-{}/{} | q:quit /:search ↑↓:scroll PgUp/PgDn:page",
            top_line + 1,
            end_line,
            lines.len()
        )
    };
    
    // Pad or truncate status to fill the terminal width
    let status_display = if status.chars().count() > term_width as usize {
        let mut truncated = status.chars()
            .take(term_width as usize - 1)
            .collect::<String>();
        truncated.push('…');
        truncated
    } else {
        // Pad with spaces to fill terminal width
        let padding = term_width as usize - status.chars().count();
        format!("{}{}", status, " ".repeat(padding))
    };
    write!(stdout, "{}", status_display.black().on_white())?;
    
    stdout.flush()?;
    Ok(())
}

fn prompt_search() -> Result<Option<String>> {
    let mut stdout = std::io::stdout();
    let (_, term_height) = terminal::size()?;
    
    // Show prompt at the bottom
    execute!(stdout, cursor::MoveTo(0, term_height - 1), terminal::Clear(ClearType::CurrentLine))?;
    write!(stdout, "/")?;
    stdout.flush()?;
    
    execute!(stdout, cursor::Show)?;
    
    let mut query = String::new();
    loop {
        if let Event::Key(KeyEvent { code, .. }) = event::read()? {
            match code {
                KeyCode::Enter => {
                    execute!(stdout, cursor::Hide)?;
                    return Ok(if query.is_empty() { None } else { Some(query) });
                }
                KeyCode::Esc => {
                    execute!(stdout, cursor::Hide)?;
                    return Ok(None);
                }
                KeyCode::Backspace => {
                    query.pop();
                    execute!(stdout, cursor::MoveTo(0, term_height - 1), terminal::Clear(ClearType::CurrentLine))?;
                    write!(stdout, "/{}", query)?;
                    stdout.flush()?;
                }
                KeyCode::Char(c) => {
                    query.push(c);
                    write!(stdout, "{}", c)?;
                    stdout.flush()?;
                }
                _ => {}
            }
        }
    }
}

fn find_search_matches(lines: &[perforce::AnnotateLine], query: &str) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            line.cl_number.to_lowercase().contains(query)
                || line.username.to_lowercase().contains(query)
                || line.date.to_lowercase().contains(query)
                || line.line_content.to_lowercase().contains(query)
        })
        .map(|(i, _)| i)
        .collect()
}
