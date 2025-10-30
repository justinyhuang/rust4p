mod perforce;

use anyhow::Result;
use clap::{Parser, Subcommand};
use itertools::Itertools;
use owo_colors::OwoColorize;
use std::collections::HashMap;

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
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Opened => cmd_opened()?,
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
