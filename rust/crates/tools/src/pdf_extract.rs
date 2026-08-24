//! Minimal PDF text extraction.
//!
//! Reads a PDF file, locates `/Contents` stream objects, decompresses with
//! flate2 when the stream uses `/FlateDecode`, and extracts text operators
//! found between `BT` / `ET` markers.

use std::io::Read as _;
use std::path::Path;

/// Extract all readable text from a PDF file.
///
/// Returns the concatenated text found inside BT/ET operators across all
/// content streams.  Non-text pages or encrypted PDFs yield an empty string
/// rather than an error.
pub fn extract_text(path: &Path) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|e| format!("failed to read PDF: {e}"))?;
    Ok(extract_text_from_bytes(&data))
}

/// Core extraction from raw PDF bytes — useful for testing without touching the
/// filesystem.
pub(crate) fn extract_text_from_bytes(data: &[u8]) -> String {
    let mut all_text = String::new();
    let mut offset = 0;

    while offset < data.len() {
        let Some(stream_start) = find_subsequence(&data[offset..], b"stream") else {
            break;
        };
        let abs_start = offset + stream_start;

        // Determine the byte offset right after "stream\r\n" or "stream\n".
        let content_start = skip_stream_eol(data, abs_start + b"stream".len());

        let Some(end_rel) = find_subsequence(&data[content_start..], b"endstream") else {
            break;
        };
        let content_end = content_start + end_rel;

        // Look backwards from "stream" for a FlateDecode hint in the object
        // dictionary.  We scan at most 512 bytes before the stream keyword.
        let dict_window_start = abs_start.saturating_sub(512);
        let dict_window = &data[dict_window_start..abs_start];
        let is_flate = find_subsequence(dict_window, b"FlateDecode").is_some();

        // Only process streams whose parent dictionary references /Contents or
        // looks like a page content stream (contains /Length).  We intentionally
        // keep this loose to cover both inline and referenced content streams.
        let raw = &data[content_start..content_end];
        let decompressed;
        let stream_bytes: &[u8] = if is_flate {
            if let Ok(buf) = inflate(raw) {
                decompressed = buf;
                &decompressed
            } else {
                offset = content_end;
                continue;
            }
        } else {
            raw
        };

        let text = extract_bt_et_text(stream_bytes);
        if !text.is_empty() {
            if !all_text.is_empty() {
                all_text.push('\n');
            }
            all_text.push_str(&text);
        }

        offset = content_end;
    }

    all_text
}

/// Inflate (zlib / deflate) compressed data via `flate2`.
fn inflate(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = flate2::read::ZlibDecoder::new(data);
    let mut buf = Vec::new();
    decoder
        .read_to_end(&mut buf)
        .map_err(|e| format!("flate2 inflate error: {e}"))?;
    Ok(buf)
}

/// Extract text from PDF content-stream operators between BT and ET markers.
///
/// Handles the common text-showing operators:
/// - `Tj`  — show a string
/// - `TJ`  — show an array of strings/numbers
/// - `'`   — move to next line and show string
/// - `"`   — set spacing, move to next line and show string
fn extract_bt_et_text(stream: &[u8]) -> String {
    let text = String::from_utf8_lossy(stream);
    let mut result = String::new();
    let mut in_bt = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "BT" {
            in_bt = true;
            continue;
        }
        if trimmed == "ET" {
            in_bt = false;
            continue;
        }
        if !in_bt {
            continue;
        }

        // Tj operator: (text) Tj
        if trimmed.ends_with("Tj") {
            if let Some(s) = extract_parenthesized_string(trimmed) {
                if !result.is_empty() && !result.ends_with('\n') {
                    result.push(' ');
                }
                result.push_str(&s);
            }
        }
        // TJ operator: [ (text) 123 (text) ] TJ
        else if trimmed.ends_with("TJ") {
            let extracted = extract_tj_array(trimmed);
            if !extracted.is_empty() {
                if !result.is_empty() && !result.ends_with('\n') {
                    result.push(' ');
                }
                result.push_str(&extracted);
            }
        }
        // ' operator: (text) '   and   " operator: aw ac (text) "
        else if is_newline_show_operator(trimmed) {
            if let Some(s) = extract_parenthesized_string(trimmed) {
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str(&s);
            }
        }
    }

    result
}

/// Returns `true` when `trimmed` looks like a `'` or `"` text-show operator.
fn is_newline_show_operator(trimmed: &str) -> bool {
    (trimmed.ends_with('\'') && trimmed.len() > 1)
        || (trimmed.ends_with('"') && trimmed.contains('('))
}

/// Pull the text from the first `(…)` group, handling escaped parens and
/// common PDF escape sequences.
fn extract_parenthesized_string(input: &str) -> Option<String> {
    let open = input.find('(')?;
    let bytes = input.as_bytes();
    let mut depth = 0;
    let mut result = String::new();
    let mut i = open;

    while i < bytes.len() {
        match bytes[i] {
            b'(' => {
                if depth > 0 {
                    result.push('(');
                }
                depth += 1;
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(result);
                }
                result.push(')');
            }
            b'\\' if i + 1 < bytes.len() => {
                i += 1;
                match bytes[i] {
                    b'n' => result.push('\n'),
                    b'r' => result.push('\r'),
                    b't' => result.push('\t'),
                    b'\\' => result.push('\\'),
                    b'(' => result.push('('),
                    b')' => result.push(')'),
                    // Octal sequences — up to 3 digits.
                    d @ b'0'..=b'7' => {
                        let mut octal = u32::from(d - b'0');
                        for _ in 0..2 {
                            if i + 1 < bytes.len()
                                && bytes[i + 1].is_ascii_digit()
                                && bytes[i + 1] <= b'7'
                            {
                                i += 1;
                                octal = octal * 8 + u32::from(bytes[i] - b'0');
                            } else {
                                break;
                            }
                        }
                        if let Some(ch) = char::from_u32(octal) {
                            result.push(ch);
                        }
                    }
                    other => result.push(char::from(other)),
                }
            }
            ch => result.push(char::from(ch)),
        }
        i += 1;
    }

    None // unbalanced
}

/// Extract concatenated strings from a TJ array like `[ (Hello) -120 (World) ] TJ`.
fn extract_tj_array(input: &str) -> String {
    let mut result = String::new();
    let Some(bracket_start) = input.find('[') else {
        return result;
    };
    let Some(bracket_end) = input.rfind(']') else {
        return result;
    };
    let inner = &input[bracket_start + 1..bracket_end];

    let mut i = 0;
    let bytes = inner.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'(' {
            // Reconstruct the parenthesized string and extract it.
            if let Some(s) = extract_parenthesized_string(&inner[i..]) {
                result.push_str(&s);
                // Skip past the closing paren.
                let mut depth = 0u32;
                for &b in &bytes[i..] {
                    i += 1;
                    if b == b'(' {
                        depth += 1;
                    } else if b == b')' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
                continue;
            }
        }
        i += 1;
    }

    result
}

/// Skip past the end-of-line marker that immediately follows the `stream`
/// keyword.  Per the PDF spec this is either `\r\n` or `\n`.
fn skip_stream_eol(data: &[u8], pos: usize) -> usize {
    if pos < data.len() && data[pos] == b'\r' {
        if pos + 1 < data.len() && data[pos + 1] == b'\n' {
            return pos + 2;
        }
        return pos + 1;
    }
    if pos < data.len() && data[pos] == b'\n' {
        return pos + 1;
    }
    pos
}

/// Simple byte-subsequence search.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Check if a user-supplied path looks like a PDF file reference.
#[must_use]
pub fn looks_like_pdf_path(text: &str) -> Option<&str> {
    for token in text.split_whitespace() {
        let cleaned = token.trim_matches(|c: char| c == '\'' || c == '"' || c == '`');
        if let Some(dot_pos) = cleaned.rfind('.') {
            if cleaned[dot_pos + 1..].eq_ignore_ascii_case("pdf") && dot_pos > 0 {
                return Some(cleaned);
            }
        }
    }
    None
}

/// Auto-extract text from a PDF path mentioned in a user prompt.
///
/// Returns `Some((path, extracted_text))` when a `.pdf` path is detected and
/// the file exists, otherwise `None`.
#[must_use]
pub fn maybe_extract_pdf_from_prompt(prompt: &str) -> Option<(String, String)> {
    let pdf_path = looks_like_pdf_path(prompt)?;
    let path = Path::new(pdf_path);
    if !path.exists() {
        return None;
    }
    let text = extract_text(path).ok()?;
    if text.is_empty() {
        return None;
    }
    Some((pdf_path.to_string(), text))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid PDF with a single page containing uncompressed
    /// text.  This is the smallest PDF structure that exercises the BT/ET
    /// extraction path.
    fn build_simple_pdf(text: &str) -> Vec<u8> {
        let content_stream = format!("BT\n/F1 12 Tf\n({text}) Tj\nET");
        let stream_bytes = content_stream.as_bytes();
        let mut pdf = Vec::new();

        // Header
        pdf.extend_from_slice(b"%PDF-1.4\n");

        // Object 1 — Catalog
        let obj1_offset = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        // Object 2 — Pages
        let obj2_offset = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        // Object 3 — Page
        let obj3_offset = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>\nendobj\n",
        );

        // Object 4 — Content stream (uncompressed)
        let obj4_offset = pdf.len();
        let length = stream_bytes.len();
        let header = format!("4 0 obj\n<< /Length {length} >>\nstream\n");
        pdf.extend_from_slice(header.as_bytes());
        pdf.extend_from_slice(stream_bytes);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        // Cross-reference table
        let xref_offset = pdf.len();
        pdf.extend_from_slice(b"xref\n0 5\n");
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{obj1_offset:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{obj2_offset:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{obj3_offset:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{obj4_offset:010} 00000 n \n").as_bytes());

        // Trailer
        pdf.extend_from_slice(b"trailer\n<< /Size 5 /Root 1 0 R >>\n");
        pdf.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

        pdf
    }

    /// Build a minimal PDF with flate-compressed content stream.
    fn build_flate_pdf(text: &str) -> Vec<u8> {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write as _;

        let content_stream = format!("BT\n/F1 12 Tf\n({text}) Tj\nET");
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(content_stream.as_bytes())
            .expect("compress");
        let compressed = encoder.finish().expect("finish");

        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let obj1_offset = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let obj2_offset = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let obj3_offset = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>\nendobj\n",
        );

        let obj4_offset = pdf.len();
        let length = compressed.len();
        let header = format!("4 0 obj\n<< /Length {length} /Filter /FlateDecode >>\nstream\n");
        pdf.extend_from_slice(header.as_bytes());
        pdf.extend_from_slice(&compressed);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        let xref_offset = pdf.len();
        pdf.extend_from_slice(b"xref\n0 5\n");
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{obj1_offset:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{obj2_offset:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{obj3_offset:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{obj4_offset:010} 00000 n \n").as_bytes());

        pdf.extend_from_slice(b"trailer\n<< /Size 5 /Root 1 0 R >>\n");
        pdf.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

        pdf
    }

    #[test]
    fn extracts_uncompressed_text_from_minimal_pdf() {
        // given
        let pdf_bytes = build_simple_pdf("Hello World");

        // when
        let text = extract_text_from_bytes(&pdf_bytes);

        // then
        assert_eq!(text, "Hello World");
    }

    #[test]
    fn extracts_text_from_flate_compressed_stream() {
        // given
        let pdf_bytes = build_flate_pdf("Compressed PDF Text");

        // when
        let text = extract_text_from_bytes(&pdf_bytes);

        // then
        assert_eq!(text, "Compressed PDF Text");
    }

    #[test]
    fn handles_tj_array_operator() {
        // given
        let stream = b"BT\n/F1 12 Tf\n[ (Hello) -120 ( World) ] TJ\nET";
        // Build a raw PDF with TJ array operator instead of simple Tj.
        let content_stream = std::str::from_utf8(stream).unwrap();
        let raw = format!(
            "%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n\
             2 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n%%EOF\n",
            content_stream.len(),
            content_stream
        );
        let pdf_bytes = raw.into_bytes();

        // when
        let text = extract_text_from_bytes(&pdf_bytes);

        // then
        assert_eq!(text, "Hello World");
    }

    #[test]
    fn handles_escaped_parentheses() {
        // given
        let content = b"BT\n(Hello \\(World\\)) Tj\nET";
        let raw = format!(
            "%PDF-1.4\n1 0 obj\n<< /Length {} >>\nstream\n",
            content.len()
        );
        let mut pdf_bytes = raw.into_bytes();
        pdf_bytes.extend_from_slice(content);
        pdf_bytes.extend_from_slice(b"\nendstream\nendobj\n%%EOF\n");

        // when
        let text = extract_text_from_bytes(&pdf_bytes);

        // then
        assert_eq!(text, "Hello (World)");
    }

    #[test]
    fn returns_empty_for_non_pdf_data() {
        // given
        let data = b"This is not a PDF file at all";

        // when
        let text = extract_text_from_bytes(data);

        // then
        assert!(text.is_empty());
    }

    #[test]
    fn extracts_text_from_file_on_disk() {
        // given
        let pdf_bytes = build_simple_pdf("Disk Test");
        let dir = std::env::temp_dir().join("clawd-pdf-extract-test");
        std::fs::create_dir_all(&dir).unwrap();
        let pdf_path = dir.join("test.pdf");
        std::fs::write(&pdf_path, &pdf_bytes).unwrap();

        // when
        let text = extract_text(&pdf_path).unwrap();

        // then
        assert_eq!(text, "Disk Test");

        // cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn looks_like_pdf_path_detects_pdf_references() {
        // given / when / then
        assert_eq!(
            looks_like_pdf_path("Please read /tmp/report.pdf"),
            Some("/tmp/report.pdf")
        );
        assert_eq!(looks_like_pdf_path("Check file.PDF now"), Some("file.PDF"));
        assert_eq!(looks_like_pdf_path("no pdf here"), None);
    }

    #[test]
    fn maybe_extract_pdf_from_prompt_returns_none_for_missing_file() {
        // given
        let prompt = "Read /tmp/nonexistent-abc123.pdf please";

        // when
        let result = maybe_extract_pdf_from_prompt(prompt);

        // then
        assert!(result.is_none());
    }

    #[test]
    fn maybe_extract_pdf_from_prompt_extracts_existing_file() {
        // given
        let pdf_bytes = build_simple_pdf("Auto Extracted");
        let dir = std::env::temp_dir().join("clawd-pdf-auto-extract-test");
        std::fs::create_dir_all(&dir).unwrap();
        let pdf_path = dir.join("auto.pdf");
        std::fs::write(&pdf_path, &pdf_bytes).unwrap();
        let prompt = format!("Summarize {}", pdf_path.display());

        // when
        let result = maybe_extract_pdf_from_prompt(&prompt);

        // then
        let (path, text) = result.expect("should extract");
        assert_eq!(path, pdf_path.display().to_string());
        assert_eq!(text, "Auto Extracted");

        // cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }
}
