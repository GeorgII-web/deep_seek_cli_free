//! The start picture: "DEEP SEEK CLI FREE" drawn with block characters.

use super::style::Style;

/// Rows of a glyph.
const HEIGHT: usize = 5;
/// Columns between two words.
const WORD_GAP: usize = 2;
const INDENT: usize = 2;

/// A letter as five rows; `#` is a filled cell.
fn glyph(letter: char) -> [&'static str; HEIGHT] {
    match letter {
        'D' => ["### ", "#  #", "#  #", "#  #", "### "],
        'E' => ["####", "#   ", "### ", "#   ", "####"],
        'P' => ["### ", "#  #", "### ", "#   ", "#   "],
        'S' => [" ###", "#   ", " ## ", "   #", "### "],
        'K' => ["#  #", "# # ", "##  ", "# # ", "#  #"],
        'C' => [" ###", "#   ", "#   ", "#   ", " ###"],
        'L' => ["#   ", "#   ", "#   ", "#   ", "####"],
        'I' => ["###", " # ", " # ", " # ", "###"],
        'F' => ["####", "#   ", "### ", "#   ", "#   "],
        'R' => ["### ", "#  #", "### ", "# # ", "#  #"],
        _ => ["    "; HEIGHT],
    }
}

/// The five rows of one word, letters one column apart.
fn word_rows(word: &str) -> Vec<String> {
    (0..HEIGHT)
        .map(|row| {
            let cells: Vec<&str> = word.chars().map(|letter| glyph(letter)[row]).collect();
            cells.join(" ").replace('#', "█")
        })
        .collect()
}

/// The picture for a terminal `columns` wide: all four words in one band, two
/// bands on a narrow terminal, nothing when even that does not fit.
/// "DEEP SEEK" is blue from light to dark, "CLI" light gray, "FREE" green.
pub fn art(style: Style, columns: usize) -> Vec<String> {
    // 256-color codes per row, top to bottom.
    const BLUE: [u8; HEIGHT] = [117, 81, 75, 69, 63];
    const GRAY: [u8; HEIGHT] = [255, 253, 251, 249, 247];
    const GREEN: [u8; HEIGHT] = [157, 120, 114, 78, 72];
    let words: [(&str, [u8; HEIGHT]); 4] = [
        ("DEEP", BLUE),
        ("SEEK", BLUE),
        ("CLI", GRAY),
        ("FREE", GREEN),
    ];

    let width = |band: &[(&str, [u8; HEIGHT])]| -> usize {
        let letters: usize = band
            .iter()
            .map(|(word, _)| word_rows(word)[0].chars().count())
            .sum();
        INDENT + letters + WORD_GAP * (band.len() - 1)
    };
    let bands: Vec<&[(&str, [u8; HEIGHT])]> = if width(&words) <= columns {
        vec![&words]
    } else if width(&words[..2]) <= columns {
        vec![&words[..2], &words[2..]]
    } else {
        return Vec::new();
    };

    let mut lines = Vec::new();
    for (i, band) in bands.iter().enumerate() {
        if i > 0 {
            lines.push(String::new());
        }
        let rows: Vec<Vec<String>> = band.iter().map(|(word, _)| word_rows(word)).collect();
        for row in 0..HEIGHT {
            let mut line = " ".repeat(INDENT);
            for (n, (_, colors)) in band.iter().enumerate() {
                if n > 0 {
                    line.push_str(&" ".repeat(WORD_GAP));
                }
                if style.on() {
                    line.push_str(&format!(
                        "\x1b[38;5;{}m{}{}",
                        colors[row],
                        rows[n][row],
                        style.reset()
                    ));
                } else {
                    line.push_str(&rows[n][row]);
                }
            }
            lines.push(line.trim_end().to_string());
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(columns: usize) -> Vec<String> {
        art(Style::new(false), columns)
    }

    #[test]
    fn one_band_when_wide_two_when_narrow_none_when_tiny() {
        let wide = plain(80);
        assert_eq!(wide.len(), HEIGHT);
        assert!(wide.iter().all(|line| line.chars().count() <= 80));
        assert_eq!(
            wide[0],
            "  ███  ████ ████ ███    ███ ████ ████ █  █   ███ █    ███  ████ ███  ████ ████"
        );

        let narrow = plain(60);
        assert_eq!(narrow.len(), 2 * HEIGHT + 1);
        assert!(narrow.iter().all(|line| line.chars().count() <= 60));
        assert!(plain(30).is_empty());
    }

    #[test]
    fn every_letter_of_the_name_has_a_glyph() {
        for letter in "DEEPSEEKCLIFREE".chars() {
            let rows = glyph(letter);
            assert!(rows.iter().any(|row| row.contains('#')), "{letter}");
            assert!(
                rows.iter().all(|row| row.len() == rows[0].len()),
                "{letter}"
            );
        }
    }

    #[test]
    fn colors_wrap_each_word() {
        let colored = art(Style::new(true), 80);
        assert!(colored[0].contains("\x1b[38;5;117m") && colored[0].contains("\x1b[38;5;157m"));
        assert!(colored[4].contains("\x1b[38;5;63m"));
    }
}
