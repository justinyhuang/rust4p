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
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Opened => cmd_opened()?,
        Commands::Change => cmd_change()?,
        Commands::Reopen => cmd_reopen()?,
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
        
        let box_width = std::cmp::max(
            visual_width(&header),
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
        let color = palette[palette_idx % palette.len()];
        palette_idx += 1;

        let title = if key == "default" {
            "CL default (pending)".to_string()
        } else {
            format!("CL {key}")
        };
        let header = format!(" {} — {} file(s) ", title, files.len());
        let is_last = idx == num_keys - 1;
        print_box(&header, &files.iter().map(render_opened_line).collect_vec(), color, max_width, idx > 0, is_last);
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
    
    // Run interactive selector
    let selected = interactive_select(&cls)?;
    
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
    
    // Map CL to color
    let cl_to_color: HashMap<String, fn(&str) -> String> = cls
        .iter()
        .enumerate()
        .map(|(idx, cl)| (cl.clone(), palette[idx % palette.len()]))
        .collect();
    
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
    
    // Show CL selector
    println!("\nSelect destination changelist:");
    let dest_cl = interactive_select(&dest_cls)?;
    
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

fn interactive_file_select(
    files: &[perforce::OpenedFile],
    cl_to_color: &HashMap<String, fn(&str) -> String>,
) -> Result<Vec<perforce::OpenedFile>> {
    let mut selected_idx = 0usize;
    let mut selected_set: std::collections::HashSet<usize> = std::collections::HashSet::new();
    
    // Get current cursor position
    let start_pos = cursor::position()?;
    
    // Enable raw mode and hide cursor
    terminal::enable_raw_mode()?;
    execute!(std::io::stdout(), cursor::Hide)?;
    
    let result = (|| -> Result<Vec<perforce::OpenedFile>> {
        let mut first_draw = true;
        
        loop {
            let mut current_row = start_pos.1;
            
            // Display header
            execute!(
                std::io::stdout(),
                cursor::MoveTo(start_pos.0, current_row),
                terminal::Clear(ClearType::CurrentLine)
            )?;
            print!("Select files (↑/↓ to navigate, Space to toggle, Enter to confirm, Esc/q to cancel):");
            current_row += 1;
            
            // Empty line
            execute!(
                std::io::stdout(),
                cursor::MoveTo(start_pos.0, current_row),
                terminal::Clear(ClearType::CurrentLine)
            )?;
            current_row += 1;
            
            // Display files
            for (idx, file) in files.iter().enumerate() {
                execute!(
                    std::io::stdout(),
                    cursor::MoveTo(start_pos.0, current_row),
                    terminal::Clear(ClearType::CurrentLine)
                )?;
                
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
                    print!("{}", color(&line).bold().to_string());
                } else {
                    print!("{}", color(&line));
                }
                current_row += 1;
            }
            
            // Empty line
            execute!(
                std::io::stdout(),
                cursor::MoveTo(start_pos.0, current_row),
                terminal::Clear(ClearType::CurrentLine)
            )?;
            current_row += 1;
            
            // Selected count
            execute!(
                std::io::stdout(),
                cursor::MoveTo(start_pos.0, current_row),
                terminal::Clear(ClearType::CurrentLine)
            )?;
            print!("Selected: {} file(s)", selected_set.len());
            current_row += 1;
            
            // Clear any remaining lines on first draw
            if first_draw {
                execute!(
                    std::io::stdout(),
                    cursor::MoveTo(start_pos.0, current_row),
                    terminal::Clear(ClearType::FromCursorDown)
                )?;
                first_draw = false;
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
                        execute!(std::io::stdout(), cursor::Show)?;
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
                        execute!(std::io::stdout(), cursor::Show)?;
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
    
    // Always disable raw mode and show cursor on exit
    terminal::disable_raw_mode()?;
    execute!(std::io::stdout(), cursor::Show)?;
    
    result
}

fn interactive_select(items: &[String]) -> Result<Option<String>> {
    let mut selected_idx = 0usize;
    
    // Get current cursor position
    let start_pos = cursor::position()?;
    
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
            
            // Display header
            print!("Select a changelist (↑/↓ to navigate, Enter to edit, Esc/q to cancel):\r\n\r\n");
            
            // Display items
            for (idx, item) in items.iter().enumerate() {
                let display = if item == "default" {
                    "CL default (pending)".to_string()
                } else if item == "new" {
                    "→ new CL".to_string()
                } else {
                    format!("CL {}", item)
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

fn print_box<F>(title: &str, lines: &[String], colorize: F, width: usize, skip_top: bool, is_last: bool)
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
    for l in lines {
        let pad = width - 2 - visual_width(l);
        println!("{}", colorize(&format!("{v} {}{:pad$} {v}", l, "", pad = pad)));
    }
    if is_last {
        println!("{}", colorize(&bot));
    }
}
