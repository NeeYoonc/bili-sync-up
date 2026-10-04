//! 漫画 CBZ 的读取侧：详情页的网页阅读器用它取「一话的页面清单」和「单页图片」。
//!
//! CBZ 是我们自己用 store（不压缩）写出来的：`0001.jpg` … `00NN.jpg` + `ComicInfo.xml`。
//! 这里只读，不修改文件；图片条目按条目名排序，序号就是阅读器里的页号（从 0 起）。

use std::io::Read;
use std::path::Path;

use anyhow::{bail, Context, Result};

/// 请求的页号超出 CBZ 里的页数（HTTP 层据此返回 400）。
#[derive(Debug, thiserror::Error)]
#[error("第 {requested} 页不存在（共 {total} 页）")]
pub struct PageOutOfRange {
    pub requested: usize,
    pub total: usize,
}

/// 阅读器里的单页信息。
#[derive(Debug, Clone, serde::Serialize)]
pub struct MangaChapterPage {
    pub index: usize,
    pub name: String,
    pub size: u64,
}

/// 一话的清单（文件路径、大小、页面列表）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct MangaChapterManifest {
    pub path: String,
    pub size_bytes: u64,
    pub pages: Vec<MangaChapterPage>,
}

fn is_image_entry(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".jpg", ".jpeg", ".png", ".webp", ".avif", ".gif", ".bmp"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

fn open_archive(path: &Path) -> Result<zip::ZipArchive<std::fs::File>> {
    let file = std::fs::File::open(path).with_context(|| format!("打开 CBZ 失败: {}", path.display()))?;
    zip::ZipArchive::new(file).with_context(|| format!("解析 CBZ 失败: {}", path.display()))
}

/// 按条目名排序后的图片条目（页号 → 条目名 + 字节数）。
fn sorted_image_names(archive: &mut zip::ZipArchive<std::fs::File>) -> Result<Vec<(String, u64)>> {
    let mut names = Vec::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .with_context(|| format!("读取 CBZ 第 {} 个条目失败", index))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        if is_image_entry(&name) {
            names.push((name, entry.size()));
        }
    }
    names.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(names)
}

/// 读一话的页面清单。
pub fn read_chapter_manifest(cbz: &Path) -> Result<MangaChapterManifest> {
    let size_bytes = std::fs::metadata(cbz)
        .with_context(|| format!("读取 CBZ 文件信息失败: {}", cbz.display()))?
        .len();
    let mut archive = open_archive(cbz)?;
    let names = sorted_image_names(&mut archive)?;
    if names.is_empty() {
        bail!("CBZ 里没有图片条目: {}", cbz.display());
    }
    let pages = names
        .into_iter()
        .enumerate()
        .map(|(index, (name, size))| MangaChapterPage { index, name, size })
        .collect();
    Ok(MangaChapterManifest {
        path: cbz.to_string_lossy().to_string(),
        size_bytes,
        pages,
    })
}

/// 读第 `index` 页（从 0 起）的图片字节与 MIME。
pub fn read_chapter_page(cbz: &Path, index: usize) -> Result<(Vec<u8>, String)> {
    let mut archive = open_archive(cbz)?;
    let names = sorted_image_names(&mut archive)?;
    let Some((name, _)) = names.get(index) else {
        return Err(PageOutOfRange {
            requested: index + 1,
            total: names.len(),
        }
        .into());
    };
    let mut entry = archive
        .by_name(name)
        .with_context(|| format!("读取 CBZ 条目失败: {}", name))?;
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry
        .read_to_end(&mut bytes)
        .with_context(|| format!("读取 CBZ 条目内容失败: {}", name))?;
    let mime = mime_guess::from_path(name)
        .first_or_octet_stream()
        .essence_str()
        .to_string();
    Ok((bytes, mime))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_entry_detection_ignores_comicinfo() {
        assert!(is_image_entry("0001.jpg"));
        assert!(is_image_entry("0010.JPEG"));
        assert!(is_image_entry("cover.png"));
        assert!(!is_image_entry("ComicInfo.xml"));
        assert!(!is_image_entry("0011.jpg.txt"));
    }

    /// 造一个 store 模式的小 CBZ，验证清单排序、单页读取与越界报错。
    #[test]
    fn manifest_and_page_roundtrip() {
        let dir = std::env::temp_dir().join("bili_sync_manga_reader_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cbz = dir.join("t.cbz");
        {
            let file = std::fs::File::create(&cbz).unwrap();
            let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
            let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            zip.start_file("ComicInfo.xml", options).unwrap();
            std::io::Write::write_all(&mut zip, b"<ComicInfo/>").unwrap();
            // 故意先写 0002 再写 0001，验证读取侧会按名字排序
            for (name, payload) in [("0002.jpg", b"BBBB".as_slice()), ("0001.jpg", b"AAAA".as_slice())] {
                zip.start_file(name, options).unwrap();
                std::io::Write::write_all(&mut zip, payload).unwrap();
            }
            zip.finish().unwrap();
        }
        let manifest = read_chapter_manifest(&cbz).unwrap();
        assert_eq!(manifest.pages.len(), 2);
        assert_eq!(manifest.pages[0].name, "0001.jpg");
        assert_eq!(manifest.pages[1].name, "0002.jpg");
        assert!(manifest.size_bytes > 0);
        let (bytes, mime) = read_chapter_page(&cbz, 0).unwrap();
        assert_eq!(bytes, b"AAAA");
        assert_eq!(mime, "image/jpeg");
        assert_eq!(read_chapter_page(&cbz, 1).unwrap().0, b"BBBB");
        assert!(read_chapter_page(&cbz, 2).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
