//! Row slices for spreadsheets, isolated in the document conversion worker.
use super::Document;
use calamine::{Data, Reader};
use serde::{Deserialize, Serialize};

pub(super) const DEFAULT_ROWS: usize = 100;
pub(super) const MAX_ROWS: usize = 1_000;
const MAX_SLICE_CELLS: usize = 100_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Selection {
    pub start_row: usize,
    pub max_rows: usize,
    pub sheet: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Page {
    pub sheet: Option<String>,
    pub sheets: Vec<String>,
    pub start_row: usize,
    pub returned_rows: usize,
    pub total_rows: usize,
    pub next_start_row: Option<usize>,
}

impl Selection {
    pub fn validate(&self) -> Result<(), String> {
        if self.start_row == 0 || !(1..=MAX_ROWS).contains(&self.max_rows) {
            return Err(format!(
                "start_row is 1-based; max_rows must be between 1 and {MAX_ROWS}"
            ));
        }
        if self
            .sheet
            .as_ref()
            .is_some_and(|sheet| sheet.trim().is_empty())
        {
            return Err("sheet must name a nonempty worksheet".into());
        }
        Ok(())
    }
}

pub(super) fn convert(
    bytes: &[u8],
    format: anydoc::Format,
    selection: &Selection,
) -> Result<Document, String> {
    selection.validate()?;
    let (sheet, sheets, total_rows, rows) = match format {
        anydoc::Format::Excel | anydoc::Format::Ods => {
            let mut workbook = calamine::open_workbook_auto_from_rs(std::io::Cursor::new(bytes))
                .map_err(|error| error.to_string())?;
            let sheets = workbook.sheet_names().to_vec();
            let sheet = selection
                .sheet
                .as_ref()
                .or(sheets.first())
                .ok_or("workbook has no worksheets")?
                .clone();
            if !sheets.contains(&sheet) {
                return Err(format!(
                    "worksheet {sheet:?} does not exist; available: {sheets:?}"
                ));
            }
            let range = workbook
                .worksheet_range(&sheet)
                .map_err(|error| error.to_string())?;
            let total_rows = range
                .end()
                .map_or(0, |(row, _)| row as usize + 1);
            let columns = range
                .end()
                .map_or(0, |(_, column)| column as usize + 1);
            let end = slice_end(selection, total_rows, columns)?;
            let rows = (selection.start_row - 1..end)
                .map(|row| {
                    (0..columns)
                        .map(|column| {
                            range
                                .get_value((row as u32, column as u32))
                                .map(cell_text)
                                .unwrap_or_default()
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            (Some(sheet), sheets, total_rows, rows)
        }
        anydoc::Format::Csv => {
            if selection.sheet.is_some() {
                return Err("CSV has no worksheets; omit sheet".into());
            }
            let (total_rows, rows) = csv_slice(bytes, selection)?;
            (None, Vec::new(), total_rows, rows)
        }
        _ => {
            return Err(
                "sheet/start_row/max_rows are supported only for CSV and spreadsheets".into(),
            );
        }
    };
    let returned_rows = rows.len();
    let next = selection.start_row - 1 + returned_rows;
    let text = serde_json::to_string_pretty(&rows).map_err(|error| error.to_string())?;
    Ok(Document {
        text,
        pdf: None,
        table: Some(Page {
            sheet,
            sheets,
            start_row: selection.start_row,
            returned_rows,
            total_rows,
            next_start_row: (next < total_rows).then_some(next + 1),
        }),
    })
}

fn csv_slice(bytes: &[u8], selection: &Selection) -> Result<(usize, Vec<Vec<String>>), String> {
    let text = match bytes {
        [0xff, 0xfe, ..] => encoding_rs::UTF_16LE.decode(bytes).0,
        [0xfe, 0xff, ..] => encoding_rs::UTF_16BE.decode(bytes).0,
        _ => {
            let bytes = bytes
                .strip_prefix(&[0xef, 0xbb, 0xbf])
                .unwrap_or(bytes);
            match std::str::from_utf8(bytes) {
                Ok(text) => std::borrow::Cow::Borrowed(text),
                Err(_) => {
                    encoding_rs::WINDOWS_1252
                        .decode(bytes)
                        .0
                }
            }
        }
    };
    let reader = |delimiter| {
        csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .delimiter(delimiter)
            .from_reader(text.as_bytes())
    };
    const DELIMITER_SAMPLE_RECORDS: usize = 16;
    let mut delimiter = b',';
    let mut best_score = (0, 0);
    for candidate in *b",;\t|" {
        let widths = reader(candidate)
            .records()
            .take(DELIMITER_SAMPLE_RECORDS)
            .filter_map(Result::ok)
            .map(|record| record.len())
            .collect::<Vec<_>>();
        let score = widths
            .iter()
            .filter(|width| **width > 1)
            .map(|width| {
                (
                    widths
                        .iter()
                        .filter(|other| *other == width)
                        .count(),
                    *width,
                )
            })
            .max()
            .unwrap_or_default();
        if score > best_score {
            best_score = score;
            delimiter = candidate;
        }
    }
    let start = selection.start_row - 1;
    let end = start.saturating_add(selection.max_rows);
    let mut total_rows = 0;
    let mut cells = 0;
    let mut rows = Vec::new();
    for (index, record) in reader(delimiter).records().enumerate() {
        let record = record.map_err(|error| error.to_string())?;
        total_rows = index + 1;
        if (start..end).contains(&index) {
            cells += record.len();
            if cells > MAX_SLICE_CELLS {
                return Err("table slice exceeds 100000 cells; reduce max_rows".into());
            }
            rows.push(
                record
                    .iter()
                    .map(str::to_owned)
                    .collect(),
            );
        }
    }
    slice_end(selection, total_rows, 0)?;
    Ok((total_rows, rows))
}

fn slice_end(selection: &Selection, total_rows: usize, columns: usize) -> Result<usize, String> {
    let start = selection.start_row - 1;
    if start > total_rows {
        return Err(format!(
            "start_row exceeds table length ({total_rows} rows)"
        ));
    }
    let end = start
        .saturating_add(selection.max_rows)
        .min(total_rows);
    if (end - start).saturating_mul(columns) > MAX_SLICE_CELLS {
        return Err("table slice exceeds 100000 cells; reduce max_rows".into());
    }
    Ok(end)
}

fn cell_text(data: &Data) -> String {
    match data {
        Data::Empty => String::new(),
        Data::DateTime(value) if !value.is_duration() => value
            .as_datetime()
            .map(|date| date.to_string())
            .unwrap_or_else(|| value.to_string()),
        _ => data.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_selection_preserves_decoded_fields_and_record_boundaries() {
        let mut bytes = vec![0xff, 0xfe];
        for value in "key;note\r\n  x  ;\" line\none;two \"\r\ny;last\r\n".encode_utf16() {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        let selection = Selection {
            sheet: None,
            start_row: 2,
            max_rows: 1,
        };
        let document = convert(&bytes, anydoc::Format::Csv, &selection).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&document.text).unwrap(),
            serde_json::json!([["  x  ", " line\none;two "]])
        );
        assert_eq!(document.table.unwrap().total_rows, 3);
        assert!(
            convert(
                &bytes,
                anydoc::Format::Csv,
                &Selection {
                    start_row: 5,
                    ..selection
                }
            )
            .is_err()
        );
    }
    use std::io::Write;

    #[test]
    fn workbook_selection_keeps_sheet_names_physical_rows_and_cached_values() {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (path, text) in [
            (
                "_rels/.rels",
                concat!(
                    "<Relationships xmlns=\"http://schemas.openxmlformats.org/",
                    "package/2006/relationships\">",
                    "<Relationship Id=\"rId1\" Target=\"xl/workbook.xml\" ",
                    "Type=\"http://schemas.openxmlformats.org/",
                    "officeDocument/2006/relationships/officeDocument\"/>",
                    "</Relationships>"
                ),
            ),
            (
                "[Content_Types].xml",
                concat!(
                    "<Types xmlns=\"http://schemas.openxmlformats.org/",
                    "package/2006/content-types\">",
                    "<Override PartName=\"/xl/workbook.xml\" ContentType=\"application/",
                    "vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/></Types>"
                ),
            ),
            (
                "xl/workbook.xml",
                concat!(
                    "<workbook xmlns=\"http://schemas.openxmlformats.org/",
                    "spreadsheetml/2006/main\" ",
                    "xmlns:r=\"http://schemas.openxmlformats.org/",
                    "officeDocument/2006/relationships\">",
                    "<sheets><sheet name=\"North\" sheetId=\"1\" r:id=\"rId1\"/>",
                    "<sheet name=\"South\" sheetId=\"2\" r:id=\"rId2\"/></sheets></workbook>"
                ),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                concat!(
                    "<Relationships xmlns=\"http://schemas.openxmlformats.org/",
                    "package/2006/relationships\">",
                    "<Relationship Id=\"rId1\" Target=\"worksheets/sheet1.xml\" ",
                    "Type=\"http://schemas.openxmlformats.org/",
                    "officeDocument/2006/relationships/worksheet\"/>",
                    "<Relationship Id=\"rId2\" Target=\"worksheets/sheet2.xml\" ",
                    "Type=\"http://schemas.openxmlformats.org/",
                    "officeDocument/2006/relationships/worksheet\"/>",
                    "</Relationships>"
                ),
            ),
            (
                "xl/worksheets/sheet1.xml",
                concat!(
                    "<worksheet xmlns=\"http://schemas.openxmlformats.org/",
                    "spreadsheetml/2006/main\">",
                    "<sheetData><row r=\"1\"><c r=\"A1\"><v>11</v></c></row>",
                    "</sheetData></worksheet>"
                ),
            ),
            (
                "xl/worksheets/sheet2.xml",
                concat!(
                    "<worksheet xmlns=\"http://schemas.openxmlformats.org/",
                    "spreadsheetml/2006/main\">",
                    "<sheetData><row r=\"2\"><c r=\"A2\"><f>4+5</f><v>9</v></c></row>",
                    "<row r=\"3\"><c r=\"A3\"><v>29</v></c></row></sheetData></worksheet>"
                ),
            ),
        ] {
            zip.start_file(path, options).unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        let bytes = zip.finish().unwrap().into_inner();
        let selection = Selection {
            sheet: Some("South".into()),
            start_row: 1,
            max_rows: 2,
        };
        let document = convert(&bytes, anydoc::Format::Excel, &selection).unwrap();
        let page = document.table.unwrap();
        assert_eq!(page.sheets, ["North", "South"]);
        assert_eq!(page.sheet.as_deref(), Some("South"));
        assert_eq!(page.total_rows, 3);
        assert_eq!(page.next_start_row, Some(3));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&document.text).unwrap(),
            serde_json::json!([[""], ["9"]])
        );
        assert!(
            convert(
                &bytes,
                anydoc::Format::Excel,
                &Selection {
                    sheet: Some("Absent".into()),
                    ..selection
                }
            )
            .is_err()
        );
    }
}
