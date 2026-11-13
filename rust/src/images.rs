use image::codecs::{jpeg::JpegDecoder, png::PngDecoder, webp::WebPDecoder};
use image::{DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, Limits};
use serde_json::{json, Value};
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Cursor, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Maximum compressed original image size.
pub(crate) const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
/// Maximum decoded original pixel count.
pub(crate) const MAX_IMAGE_PIXELS: u64 = 40_000_000;
/// Maximum encoded preview size.
pub(crate) const MAX_PREVIEW_BYTES: usize = 512 * 1024;
/// Maximum preview width or height.
pub(crate) const MAX_PREVIEW_SIDE: u32 = 2048;
const MAX_DECODE_BYTES: u64 = 128 * 1024 * 1024;

/// Original provenance and a bounded, metadata-free PNG preview.
pub(crate) struct Prepared {
    /// Unmodified original bytes for artifact publication or explicit references.
    pub original: Vec<u8>,
    /// Original digest, MIME type, dimensions, and byte length.
    pub metadata: Value,
    /// Orientation-normalized model-visible PNG bytes.
    pub preview: Vec<u8>,
}

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_PIXELS as u32);
    limits.max_image_height = Some(MAX_IMAGE_PIXELS as u32);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    limits
}

fn validate_webp(bytes: &[u8]) -> Result<(), String> {
    let invalid = || "invalid WebP chunk length".to_string();
    if bytes.len() < 12
        || u64::from(u32::from_le_bytes(bytes[4..8].try_into().unwrap())) + 8 != bytes.len() as u64
    {
        return Err(invalid());
    }
    let mut offset = 12;
    while offset < bytes.len() {
        let header = bytes.get(offset..offset + 8).ok_or_else(invalid)?;
        let length = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        let end = offset
            .checked_add(8)
            .and_then(|n| n.checked_add(length))
            .ok_or_else(invalid)?;
        let data = bytes.get(offset + 8..end).ok_or_else(invalid)?;
        if &header[..4] == b"ANIM"
            || &header[..4] == b"ANMF"
            || (&header[..4] == b"VP8X" && data.first().is_some_and(|flags| flags & 2 != 0))
        {
            return Err("animated images are not supported".into());
        }
        offset = end
            .checked_add(length % 2)
            .filter(|n| *n <= bytes.len())
            .ok_or_else(invalid)?;
    }
    Ok(())
}

fn decoder(bytes: &[u8]) -> Result<(Box<dyn ImageDecoder + '_>, &'static str), String> {
    let cursor = Cursor::new(bytes);
    let (mut decoder, mime): (Box<dyn ImageDecoder>, _) =
        match image::guess_format(bytes).map_err(|e| format!("identify image: {e}"))? {
            ImageFormat::Png => {
                let decoder = PngDecoder::with_limits(cursor, limits())
                    .map_err(|e| format!("read PNG: {e}"))?;
                if decoder.is_apng().map_err(|e| format!("inspect PNG: {e}"))? {
                    return Err("animated images are not supported".into());
                }
                (Box::new(decoder), "image/png")
            }
            ImageFormat::Jpeg => (
                Box::new(JpegDecoder::new(cursor).map_err(|e| format!("read JPEG: {e}"))?),
                "image/jpeg",
            ),
            ImageFormat::WebP => {
                // The decoder trusts declared metadata lengths when allocating EXIF buffers.
                validate_webp(bytes)?;
                let decoder = WebPDecoder::new(cursor).map_err(|e| format!("read WebP: {e}"))?;
                if decoder.has_animation() {
                    return Err("animated images are not supported".into());
                }
                (Box::new(decoder), "image/webp")
            }
            _ => return Err("only PNG, JPEG, and WebP images are supported".into()),
        };
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err("image exceeds the 40-megapixel limit or has empty dimensions".into());
    }
    if decoder.total_bytes() > MAX_DECODE_BYTES {
        return Err("image exceeds the decoder allocation limit".into());
    }
    decoder
        .set_limits(limits())
        .map_err(|e| format!("limit image decoder: {e}"))?;
    Ok((decoder, mime))
}

struct PreviewWriter {
    bytes: Vec<u8>,
    full: bool,
}

impl Write for PreviewWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_PREVIEW_BYTES - self.bytes.len() {
            self.full = true;
            return Err(io::Error::other("preview byte limit exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Validates original bytes and prepares a bounded orientation-normalized PNG.
pub(crate) fn prepare(bytes: &[u8]) -> Result<Prepared, String> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("image exceeds the 20 MiB compressed-byte limit".into());
    }
    let (mut decoder, mime) = decoder(bytes)?;
    let (width, height) = decoder.dimensions();
    let orientation = decoder
        .orientation()
        .map_err(|e| format!("read image orientation: {e}"))?;
    let mut image =
        DynamicImage::from_decoder(decoder).map_err(|e| format!("decode image: {e}"))?;
    // Shrink before orientation or sample conversion so neither copies a full-size image.
    if width.max(height) > MAX_PREVIEW_SIDE {
        image = image.thumbnail(MAX_PREVIEW_SIDE, MAX_PREVIEW_SIDE);
    }
    image.apply_orientation(orientation);
    let mut image = image.into_rgba8();
    let preview = loop {
        let mut writer = PreviewWriter {
            bytes: Vec::new(),
            full: false,
        };
        let encoded = image::codecs::png::PngEncoder::new(&mut writer).write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        );
        if encoded.is_ok() {
            break writer.bytes;
        }
        if !writer.full {
            return Err(format!("encode image preview: {}", encoded.unwrap_err()));
        }
        image = image::imageops::thumbnail(
            &image,
            (image.width() * 3 / 4).max(1),
            (image.height() * 3 / 4).max(1),
        );
    };
    Ok(Prepared {
        original: bytes.to_vec(),
        metadata: json!({
            "sha256": crate::workspace::digest(bytes), "mime_type": mime,
            "width": width, "height": height, "size_bytes": bytes.len(),
        }),
        preview,
    })
}

fn io_error(action: &str, error: io::Error) -> String {
    format!("{action}: {}", crate::errors::stable_io(&error))
}

fn open_at(directory: &File, name: &str, flags: i32, mode: u32) -> io::Result<File> {
    let name = CString::new(name).map_err(io::Error::other)?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            // C variadic arguments need integer promotion; macOS mode_t is u16.
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn child_directory(parent: &File, name: &str, create: bool, private: bool) -> Result<File, String> {
    if create {
        let name_c = CString::new(name).unwrap();
        let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name_c.as_ptr(), 0o700) };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(io_error("create image cache directory", error));
            }
        }
    }
    let directory = open_at(parent, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)
        .map_err(|e| io_error("open image cache directory", e))?;
    let info = directory
        .metadata()
        .map_err(|e| io_error("inspect image cache directory", e))?;
    if info.uid() != unsafe { libc::geteuid() }
        || info.permissions().mode() & 0o022 != 0
        || (private && info.permissions().mode() & 0o777 != 0o700)
    {
        return Err(
            "image cache directory must be owned by the current user and private (0700)".into(),
        );
    }
    Ok(directory)
}

fn cache_directory(root: &Path, create: bool) -> Result<File, String> {
    if create {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)
            .map_err(|e| io_error("create platform cache directory", e))?;
    }
    // Platform cache roots may legitimately traverse symlinks, including macOS /var.
    let root =
        fs::canonicalize(root).map_err(|e| io_error("resolve platform cache directory", e))?;
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(root)
        .map_err(|e| io_error("open platform cache directory", e))?;
    let reagent = child_directory(&root, "reagent", create, false)?;
    child_directory(&reagent, "images", create, true)
}

fn descriptor(bytes: &[u8]) -> Result<Value, String> {
    if bytes.is_empty() || bytes.len() > MAX_PREVIEW_BYTES {
        return Err("preview exceeds its encoded-byte limit".into());
    }
    let (decoder, mime) = decoder(bytes)?;
    let (width, height) = decoder.dimensions();
    if mime != "image/png" || width.max(height) > MAX_PREVIEW_SIDE {
        return Err("preview must be a PNG bounded to 2048 pixels per side".into());
    }
    Ok(json!({
        "sha256": crate::workspace::digest(bytes), "mime_type": mime,
        "width": width, "height": height, "size_bytes": bytes.len(),
    }))
}

fn preview_name(preview: &Value) -> Result<String, String> {
    let invalid = || "invalid immutable image preview descriptor".to_string();
    let object = preview.as_object().ok_or_else(invalid)?;
    let hash = preview["sha256"].as_str().ok_or_else(invalid)?;
    if object.len() != 5
        || hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || preview["mime_type"] != "image/png"
        || !preview["width"]
            .as_u64()
            .is_some_and(|n| (1..=u64::from(MAX_PREVIEW_SIDE)).contains(&n))
        || !preview["height"]
            .as_u64()
            .is_some_and(|n| (1..=u64::from(MAX_PREVIEW_SIDE)).contains(&n))
        || !preview["size_bytes"]
            .as_u64()
            .is_some_and(|n| (1..=MAX_PREVIEW_BYTES as u64).contains(&n))
    {
        return Err(invalid());
    }
    Ok(format!("{hash}.png"))
}

fn load_from(directory: &File, preview: &Value) -> Result<Vec<u8>, String> {
    let name = preview_name(preview)?;
    let file = open_at(directory, &name, libc::O_RDONLY | libc::O_NONBLOCK, 0)
        .map_err(|e| io_error("open immutable image preview", e))?;
    let info = file
        .metadata()
        .map_err(|e| io_error("inspect image preview", e))?;
    if !info.is_file()
        || info.permissions().mode() & 0o777 != 0o600
        || info.uid() != unsafe { libc::geteuid() }
        || Some(info.len()) != preview["size_bytes"].as_u64()
    {
        return Err("image preview must be an intact private regular file (0600)".into());
    }
    let mut bytes = Vec::new();
    file.take((MAX_PREVIEW_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| io_error("read immutable image preview", e))?;
    if descriptor(&bytes)? != *preview {
        return Err("immutable image preview digest or dimensions do not match".into());
    }
    Ok(bytes)
}

fn store_in(directory: &File, prepared: &Prepared) -> Result<Value, String> {
    let preview = descriptor(&prepared.preview)?;
    let name = preview_name(&preview)?;
    let temporary = format!(".{}", crate::types::new_id());
    let mut file = open_at(
        directory,
        &temporary,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        0o600,
    )
    .map_err(|e| io_error("create immutable image preview", e))?;
    let temporary = CString::new(temporary).unwrap();
    let result = (|| {
        file.write_all(&prepared.preview)
            .map_err(|e| io_error("write image preview", e))?;
        file.sync_all()
            .map_err(|e| io_error("sync image preview", e))?;
        let name_c = CString::new(name).unwrap();
        let result = unsafe {
            libc::linkat(
                directory.as_raw_fd(),
                temporary.as_ptr(),
                directory.as_raw_fd(),
                name_c.as_ptr(),
                0,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(io_error("publish immutable image preview", error));
            }
        }
        // A concurrent publisher or preexisting blob is usable only if it matches exactly.
        load_from(directory, &preview)?;
        directory
            .sync_all()
            .map_err(|e| io_error("sync image cache directory", e))?;
        Ok(preview)
    })();
    unsafe {
        libc::unlinkat(directory.as_raw_fd(), temporary.as_ptr(), 0);
    }
    result
}

/// Persists a preview without overwriting any existing content-addressed blob.
pub(crate) fn store(prepared: &Prepared) -> Result<Value, String> {
    let root = crate::cache::directory().ok_or("user cache directory is unavailable")?;
    store_in(&cache_directory(&root, true)?, prepared)
}

/// Reads and verifies an admitted preview without consulting the original workspace path.
pub(crate) fn load(preview: &Value) -> Result<Vec<u8>, String> {
    preview_name(preview)?;
    let root = crate::cache::directory().ok_or("user cache directory is unavailable")?;
    load_from(&cache_directory(&root, false)?, preview)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::GenericImageView;
    use std::os::unix::fs::symlink;

    fn encode(encoder: impl ImageEncoder, width: u32, height: u32, exif: bool) {
        let mut encoder = encoder;
        if exif {
            encoder
                .set_exif_metadata(vec![
                    b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 1, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0,
                    0, 0, 0,
                ])
                .unwrap();
        }
        let mut state = 0x12345678u32;
        let pixels: Vec<u8> = (0..width as usize * height as usize * 3)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect();
        encoder
            .write_image(&pixels, width, height, image::ExtendedColorType::Rgb8)
            .unwrap();
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        encode(
            image::codecs::png::PngEncoder::new(&mut bytes),
            width,
            height,
            false,
        );
        bytes
    }

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::from((data.len() as u32).to_be_bytes());
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(data);
        let mut crc = !0u32;
        for byte in &bytes[4..] {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
            }
        }
        bytes.extend_from_slice(&(!crc).to_be_bytes());
        bytes
    }

    #[test]
    fn supported_formats_preserve_originals_and_normalize_orientation_without_metadata() {
        for (format, mime) in [
            (ImageFormat::Png, "image/png"),
            (ImageFormat::Jpeg, "image/jpeg"),
            (ImageFormat::WebP, "image/webp"),
        ] {
            let mut bytes = Vec::new();
            match format {
                ImageFormat::Png => {
                    encode(image::codecs::png::PngEncoder::new(&mut bytes), 2, 3, true)
                }
                ImageFormat::Jpeg => encode(
                    image::codecs::jpeg::JpegEncoder::new(&mut bytes),
                    2,
                    3,
                    true,
                ),
                ImageFormat::WebP => encode(
                    image::codecs::webp::WebPEncoder::new_lossless(&mut bytes),
                    2,
                    3,
                    true,
                ),
                _ => unreachable!(),
            }
            let prepared = prepare(&bytes).unwrap();
            assert_eq!(prepared.original, bytes);
            assert_eq!(
                prepared.metadata,
                json!({"sha256": crate::workspace::digest(&bytes), "mime_type": mime, "width": 2, "height": 3, "size_bytes": bytes.len()})
            );
            let mut decoder = PngDecoder::new(Cursor::new(&prepared.preview)).unwrap();
            assert_eq!(decoder.dimensions(), (3, 2), "{format:?}");
            assert!(decoder.exif_metadata().unwrap().is_none());
            assert_eq!(
                decoder.orientation().unwrap(),
                image::metadata::Orientation::NoTransforms
            );
        }
    }

    #[test]
    fn previews_bound_dimensions_and_encoded_bytes_without_upscaling() {
        for (width, height) in [(4, 3), (4096, 32), (1024, 1024)] {
            let prepared = prepare(&png(width, height)).unwrap();
            assert!(prepared.preview.len() <= MAX_PREVIEW_BYTES);
            let preview = image::load_from_memory(&prepared.preview).unwrap();
            assert!(preview.width() <= width.min(MAX_PREVIEW_SIDE));
            assert!(preview.height() <= height.min(MAX_PREVIEW_SIDE));
            if width == 4 {
                assert_eq!(preview.dimensions(), (width, height));
            }
            if width == 1024 {
                assert!(
                    preview.width() < width,
                    "incompressible pixels must shrink for byte budget"
                );
            }
        }
    }

    #[test]
    fn malformed_unsupported_animated_and_oversized_inputs_are_rejected() {
        for bytes in [
            Vec::new(),
            b"not an image".to_vec(),
            b"GIF89a\x01\0\x01\0".to_vec(),
            png(1, 1)[..40].to_vec(),
        ] {
            assert!(prepare(&bytes).is_err());
        }
        assert!(prepare(&vec![0; MAX_IMAGE_BYTES + 1])
            .err()
            .unwrap()
            .contains("20 MiB"));
        let mut at_limit = png(1, 1);
        at_limit.resize(MAX_IMAGE_BYTES, 0);
        assert!(prepare(&at_limit).is_ok());
        let base = png(1, 1);
        for (width, height, bit_depth, expected) in [
            (8001u32, 5000u32, 8, "40-megapixel"),
            (5000, 5000, 16, "allocation"),
        ] {
            let mut header = base[16..29].to_vec();
            header[..4].copy_from_slice(&width.to_be_bytes());
            header[4..8].copy_from_slice(&height.to_be_bytes());
            header[8] = bit_depth;
            let mut bytes = base.clone();
            bytes.splice(8..33, chunk(b"IHDR", &header));
            assert!(prepare(&bytes).err().unwrap().contains(expected));
        }
        let mut animated = base;
        animated.splice(33..33, chunk(b"acTL", &[0, 0, 0, 1, 0, 0, 0, 0]));
        assert!(prepare(&animated).err().unwrap().contains("animated"));
        let mut webp = Vec::new();
        encode(
            image::codecs::webp::WebPEncoder::new_lossless(&mut webp),
            1,
            1,
            true,
        );
        assert_eq!(&webp[12..16], b"VP8X");
        let mut animated = webp.clone();
        animated[20] |= 2;
        assert!(prepare(&animated).err().unwrap().contains("animated"));
        // A tiny input must not allocate a maliciously declared multi-gigabyte EXIF chunk.
        let offset = webp.windows(4).position(|bytes| bytes == b"EXIF").unwrap();
        webp[offset + 4..offset + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(prepare(&webp).err().unwrap().contains("chunk length"));
    }

    #[test]
    fn cache_is_private_immutable_and_independent_of_source_bytes() {
        let root = tempfile::tempdir().unwrap();
        let alias = root.path().join("alias");
        symlink(root.path(), &alias).unwrap();
        let directory = cache_directory(&alias, true).unwrap();
        let mut prepared = prepare(&png(4, 3)).unwrap();
        let expected = prepared.preview.clone();
        let barrier = std::sync::Barrier::new(4);
        let previews = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        store_in(&directory, &prepared).unwrap()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        let preview = previews[0].clone();
        assert!(previews.iter().all(|item| *item == preview));
        assert_eq!(store_in(&directory, &prepared).unwrap(), preview);
        prepared.original.clear();
        prepared.preview.clear();
        assert_eq!(load_from(&directory, &preview).unwrap(), expected);
        let blob = root
            .path()
            .join("reagent/images")
            .join(preview_name(&preview).unwrap());
        assert_eq!(
            fs::metadata(blob.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&blob).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(blob.parent().unwrap()).unwrap().count(), 1);
        let mut wrong_dimensions = preview.clone();
        wrong_dimensions["width"] = json!(1);
        assert!(load_from(&directory, &wrong_dimensions).is_err());
        let mut corrupt = expected;
        *corrupt.last_mut().unwrap() ^= 1;
        fs::write(&blob, &corrupt).unwrap();
        assert!(load_from(&directory, &preview).is_err());
        let prepared = prepare(&png(4, 3)).unwrap();
        assert!(store_in(&directory, &prepared).is_err());
        assert_eq!(
            fs::read(&blob).unwrap(),
            corrupt,
            "corrupt blobs must not be repaired silently"
        );
        fs::remove_file(&blob).unwrap();
        assert!(load_from(&directory, &preview).is_err());
        symlink(root.path().join("outside"), &blob).unwrap();
        assert!(load_from(&directory, &preview).is_err());
        assert!(store_in(&directory, &prepared).is_err());
    }

    #[test]
    fn unsafe_cache_directories_and_descriptors_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let directory = cache_directory(root.path(), true).unwrap();
        let prepared = prepare(&png(2, 2)).unwrap();
        let preview = store_in(&directory, &prepared).unwrap();
        for (key, value) in [
            ("sha256", json!("../outside")),
            ("mime_type", json!("image/jpeg")),
            ("width", json!(0)),
            ("height", json!(2049)),
            ("size_bytes", json!(MAX_PREVIEW_BYTES + 1)),
            ("path", json!("/arbitrary")),
        ] {
            let mut invalid = preview.clone();
            invalid[key] = value;
            assert!(load_from(&directory, &invalid).is_err(), "{invalid}");
        }
        let images = root.path().join("reagent/images");
        fs::set_permissions(&images, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(cache_directory(root.path(), true).is_err());
        fs::set_permissions(&images, fs::Permissions::from_mode(0o700)).unwrap();
        fs::rename(&images, root.path().join("other")).unwrap();
        symlink(root.path().join("other"), &images).unwrap();
        assert!(cache_directory(root.path(), true).is_err());
        fs::remove_file(images).unwrap();
        let reagent = root.path().join("reagent");
        fs::remove_dir(&reagent).unwrap();
        symlink(root.path().join("other"), reagent).unwrap();
        assert!(cache_directory(root.path(), true).is_err());
    }
}
