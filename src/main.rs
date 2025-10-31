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
    /// Diff files in a changelist.
    Diff,
    /// Open a file for edit in a specific changelist.
    #[command(name = "open")]
    Open {
        /// Path to the file to open
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
        Commands::Diff => cmd_diff()?,
        Commands::Open { file } => cmd_open(&file)?,
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

    // Calculate max width across all boxes first
    let mut max_width = 0usize;
    for key in &keys {
        let files = map.get(key).unwrap();
        let title = if key == "default" {
            "CL default (pending)".to_string()
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
        } else {
            format!("CL {key}")
        };
        let header = format!(" {} — {} file(s) ", title, files.len());
        let description = cl_descriptions.get(key).map(|s| s.as_str()).unwrap_or("");
        let is_last = idx == num_keys - 1;
        print_box(&header, description, &files.iter().map(render_opened_line).collect_vec(), color, max_width, idx > 0, is_last);
    }

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
    
    if cls.is_empty() {
        println!("No open changelists found.");
        return Ok(());
    }
    
    // Fetch descriptions for each CL
    let mut cl_descriptions: HashMap<String, String> = HashMap::new();
    for cl in &cls {
        if cl != "default" {
            if let Ok(Some(desc)) = perforce::get_change_description(cl) {
                // Get first line of description
                let first_line = desc.lines().next().unwrap_or("").trim();
                cl_descriptions.insert(cl.clone(), first_line.to_string());
            }
        }
    }
    
    // Run interactive selector
    let selected = interactive_select_with_desc(&cls, &cl_descriptions)?;
    
    if let Some(cl) = selected {
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
    
    // Print newline to establish starting position
    println!();
    
    // Interactive file selector
    let selected_files = interactive_file_select(&opened, &cl_to_color)?;
    
    if selected_files.is_empty() {
        println!("No files selected.");
        return Ok(());
    }
    
    // Get unique CLs for destination selection
    let mut dest_cls: Vec<String> = opened
        .iter()
        .map(|f| f.changelist.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    
    // Sort CLs
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
    
    // Print newline to establish starting position
    println!();
    
    // Interactive file selector
    let selected_files = interactive_file_select(&opened, &cl_to_color)?;
    
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

fn cmd_open(file_path: &str) -> Result<()> {
    // Get the depot path for the given file
    let depot_path = match perforce::get_depot_path(file_path)? {
        Some(path) => path,
        None => {
            eprintln!("Error: File '{}' is not in the Perforce workspace or doesn't exist.", file_path);
            return Ok(());
        }
    };

    println!("Depot path: {}", depot_path.bright_cyan());
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
    
    println!("Select a changelist to open the file to:");
    println!();
    
    let selected_cl = match interactive_select_with_desc(&cls, &cl_descriptions)? {
        Some(cl) => cl,
        None => {
            println!("No changelist selected.");
            return Ok(());
        }
    };
    
    // Run p4 edit -c <CL> <depot_path>
    let output = std::process::Command::new("p4")
        .arg("edit")
        .arg("-c")
        .arg(&selected_cl)
        .arg(&depot_path)
        .output()?;
    
    if output.status.success() {
        println!("\n{}", "File opened successfully!".bright_green());
        print!("{}", String::from_utf8_lossy(&output.stdout));
    } else {
        eprintln!("\n{}", "Error opening file:".bright_red());
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    }

    Ok(())
}

fn cmd_unshelve() -> Result<()> {
    println!("Enter CL number to unshelve:");
    
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let cl_number = input.trim();
    
    if cl_number.is_empty() {
        println!("Error: No CL number provided");
        return Ok(());
    }
    
    // Validate it's a number
    if cl_number.parse::<i64>().is_err() {
        println!("Error: Invalid CL number '{}'", cl_number);
        return Ok(());
    }
    
    // Check if CL exists and get description
    match perforce::get_change_description(cl_number)? {
        None => {
            println!("Error: CL {} does not exist", cl_number);
            return Ok(());
        }
        Some(desc) => {
            println!("\nCL {} found:", cl_number);
            let first_line = desc.lines().next().unwrap_or("(no description)");
            println!("Description: {}", first_line);
        }
    }
    
    // Unshelve the files
    println!("\nUnshelving CL {}...", cl_number);
    match perforce::unshelve_changelist(cl_number) {
        Ok(_) => {
            println!("✓ Successfully unshelved files from CL {}", cl_number);
        }
        Err(e) => {
            eprintln!("Error unshelving: {}", e);
            return Err(e);
        }
    }
    
    // Reopen files to the same CL
    println!("\nReopening unshelved files to CL {}...", cl_number);
    
    // Get all opened files
    let opened = perforce::get_opened_files()?;
    
    // Filter files that are in the default changelist (just unshelved)
    let default_files: Vec<_> = opened
        .iter()
        .filter(|f| f.changelist == "default")
        .collect();
    
    if default_files.is_empty() {
        println!("No files found in default changelist to reopen");
        return Ok(());
    }
    
    println!("Reopening {} file(s) to CL {}...", default_files.len(), cl_number);
    
    for file in default_files {
        let mut cmd = std::process::Command::new("p4");
        cmd.arg("reopen").arg("-c").arg(cl_number).arg(&file.depot_file);
        
        let output = cmd.output()?;
        if !output.status.success() {
            eprintln!("Warning: Failed to reopen {}: {}", 
                file.depot_file, 
                String::from_utf8_lossy(&output.stderr));
        } else {
            println!("✓ {}", file.depot_file);
        }
    }
    
    println!("\nDone! CL {} is ready for use.", cl_number);
    
    Ok(())
}

fn interactive_file_select(
    files: &[perforce::OpenedFile],
    cl_to_color: &HashMap<String, fn(&str) -> String>,
) -> Result<Vec<perforce::OpenedFile>> {
    let mut selected_idx = 0usize;
    let mut selected_set: std::collections::HashSet<usize> = std::collections::HashSet::new();
    
    // Get terminal size and current cursor position
    let (_term_width, term_height) = terminal::size()?;
    let initial_pos = cursor::position()?;
    
    // Calculate how many lines we need (header + blank + files + blank + footer)
    let needed_lines = 5 + files.len();
    let available_lines = (term_height - initial_pos.1) as usize;
    
    // If we don't have enough space, move cursor up
    let start_pos = if available_lines < needed_lines {
        // Move cursor to a position where we have enough space
        let new_row = term_height.saturating_sub(needed_lines as u16 + 1);
        execute!(
            std::io::stdout(), 
            cursor::MoveTo(0, new_row),
            terminal::Clear(ClearType::FromCursorDown)
        )?;
        std::io::stdout().flush()?;
        cursor::position()?
    } else {
        initial_pos
    };
    
    // Enable raw mode
    terminal::enable_raw_mode()?;
    
    let result = (|| -> Result<Vec<perforce::OpenedFile>> {
        loop {
            // Move cursor to start position and clear from here down
            execute!(
                std::io::stdout(),
                cursor::MoveTo(start_pos.0, start_pos.1),
                terminal::Clear(ClearType::FromCursorDown)
            )?;
            std::io::stdout().flush()?;
            
            // Display header
            print!("Select files (↑/↓ to navigate, Space to toggle, Enter to confirm, Esc/q to cancel):\r\n\r\n");
            
            // Display files
            for (idx, file) in files.iter().enumerate() {
                let color = cl_to_color.get(&file.changelist).unwrap();
                let cl_label = if file.changelist == "default" {
                    "default".to_string()
                } else {
                    file.changelist.clone()
                };
                
                let checkbox = if selected_set.contains(&idx) { "[✓]" } else { "[ ]" };
                let arrow = if idx == selected_idx { "→" } else { " " };
                
                let line = format!("  {}  {} CL {:8} {}", 
                    arrow, checkbox, cl_label, file.depot_file);
                
                if idx == selected_idx {
                    print!("{}\r\n", color(&line).bold().to_string());
                } else {
                    print!("{}\r\n", color(&line));
                }
            }
            
            print!("\r\n");
            print!("Selected: {} file(s)\r\n", selected_set.len());
            
            std::io::stdout().flush()?;
            
            // Read key event
            if let Event::Key(KeyEvent { code, .. }) = event::read()? {
                match code {
                    KeyCode::Up => {
                        if selected_idx > 0 {
                            selected_idx -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if selected_idx < files.len() - 1 {
                            selected_idx += 1;
                        }
                    }
                    KeyCode::Char(' ') => {
                        if selected_set.contains(&selected_idx) {
                            selected_set.remove(&selected_idx);
                        } else {
                            selected_set.insert(selected_idx);
                        }
                    }
                    KeyCode::Enter => {
                        terminal::disable_raw_mode()?;
                        // Clear the menu
                        execute!(
                            std::io::stdout(),
                            cursor::MoveTo(start_pos.0, start_pos.1),
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
                            cursor::MoveTo(start_pos.0, start_pos.1),
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

fn interactive_select_with_desc(items: &[String], descriptions: &HashMap<String, String>) -> Result<Option<String>> {
    let mut selected_idx = 0usize;
    
    // Get terminal size and current cursor position
    let (_term_width, term_height) = terminal::size()?;
    let initial_pos = cursor::position()?;
    
    // Calculate how many lines we need (header + blank + items)
    let needed_lines = 3 + items.len();
    let available_lines = (term_height - initial_pos.1) as usize;
    
    // If we don't have enough space, move cursor up or clear screen
    let start_pos = if available_lines < needed_lines {
        // Move cursor to a position where we have enough space
        let new_row = term_height.saturating_sub(needed_lines as u16 + 1);
        execute!(
            std::io::stdout(), 
            cursor::MoveTo(0, new_row),
            terminal::Clear(ClearType::FromCursorDown)
        )?;
        std::io::stdout().flush()?;
        cursor::position()?
    } else {
        initial_pos
    };
    
    // Enable raw mode
    terminal::enable_raw_mode()?;
    
    let result = (|| -> Result<Option<String>> {
        loop {
            // Move cursor to start position and clear from here down
            execute!(
                std::io::stdout(),
                cursor::MoveTo(start_pos.0, start_pos.1),
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
            
            // Read key event
            if let Event::Key(KeyEvent { code, .. }) = event::read()? {
                match code {
                    KeyCode::Up => {
                        if selected_idx > 0 {
                            selected_idx -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if selected_idx < items.len() - 1 {
                            selected_idx += 1;
                        }
                    }
                    KeyCode::Enter => {
                        let result = items[selected_idx].clone();
                        terminal::disable_raw_mode()?;
                        // Clear the menu and print final selection
                        execute!(
                            std::io::stdout(),
                            cursor::MoveTo(start_pos.0, start_pos.1),
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
                            cursor::MoveTo(start_pos.0, start_pos.1),
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
