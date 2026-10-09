use agent_client_protocol::schema as acp;
use anyhow::{Context as _, Result};
use base64::Engine as _;
use pdf_extract::{Dictionary, Document, Object, ObjectId, Stream, content::Content, dictionary};
use std::{collections::HashSet, path::Path};

const MAX_PDF_BYTES: usize = 50 * 1024 * 1024;
const MAX_CONTEXT_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
const MAX_IMAGE_EDGE: f32 = 1568.0;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PdfImageContext {
    pub data: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PdfPageContext {
    pub number: u32,
    pub text: String,
    pub images: Vec<PdfImageContext>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PdfContext {
    pub pages: Vec<PdfPageContext>,
}

impl PdfContext {
    pub fn ensure_image_support(&self, supports_images: bool) -> Result<()> {
        anyhow::ensure!(
            supports_images || self.pages.iter().all(|page| page.images.is_empty()),
            "Este PDF contem imagens, mas o agente/modelo selecionado nao aceita imagens. Escolha um modelo com visao para enviar o anexo completo."
        );
        Ok(())
    }

    pub fn text_context(&self) -> String {
        let mut text = format!(
            "PDF attachment: {} pages. Extracted text is supplied below as text. \
             Embedded images are attached separately and identified by page and image number.\n",
            self.pages.len()
        );
        for page in &self.pages {
            text.push_str(&format!("\n--- PDF page {} ---\n", page.number));
            text.push_str(&page.text);
            text.push('\n');
        }
        text
    }

    pub fn content_blocks(
        &self,
        name: &str,
        uri: &str,
        supports_embedded_context: bool,
        supports_images: bool,
    ) -> Result<Vec<acp::ContentBlock>> {
        self.ensure_image_support(supports_images)?;
        let mut blocks = Vec::new();
        let text = self.text_context();
        if supports_embedded_context {
            blocks.push(acp::ContentBlock::Resource(acp::EmbeddedResource::new(
                acp::EmbeddedResourceResource::TextResourceContents(
                    acp::TextResourceContents::new(text, uri),
                ),
            )));
        } else {
            blocks.push(acp::ContentBlock::ResourceLink(acp::ResourceLink::new(
                name, uri,
            )));
            blocks.push(acp::ContentBlock::Text(acp::TextContent::new(text)));
        }
        for page in &self.pages {
            for (index, image) in page.images.iter().enumerate() {
                blocks.push(acp::ContentBlock::Text(acp::TextContent::new(format!(
                    "PDF {name}: image {} from page {}.",
                    index + 1,
                    page.number,
                ))));
                blocks.push(acp::ContentBlock::Image(
                    acp::ImageContent::new(image.data.clone(), "image/png").uri(Some(format!(
                        "{uri}#page={}&image={}",
                        page.number,
                        index + 1
                    ))),
                ));
            }
        }
        Ok(blocks)
    }
}

pub fn is_pdf_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

pub fn extract_pdf_context(bytes: &[u8]) -> Result<PdfContext> {
    anyhow::ensure!(
        bytes.len() <= MAX_PDF_BYTES,
        "O PDF excede o limite de 50 MB por anexo."
    );
    let document = Document::load_mem(bytes).context("Nao foi possivel abrir o PDF.")?;
    anyhow::ensure!(
        !document.is_encrypted(),
        "Este PDF esta protegido por senha."
    );
    let page_ids = document.get_pages();
    anyhow::ensure!(!page_ids.is_empty(), "O PDF nao possui paginas.");
    let texts = pdf_extract::extract_text_from_mem_by_pages(bytes)
        .context("Nao foi possivel extrair o texto do PDF.")?;
    anyhow::ensure!(
        texts.len() == page_ids.len(),
        "Extracao incompleta do texto do PDF."
    );
    let mut pages = Vec::with_capacity(page_ids.len());
    let mut image_bytes = 0;
    for ((number, page_id), text) in page_ids.into_iter().zip(texts) {
        let resources = page_resources(&document, page_id)?;
        let mut sources = Vec::new();
        collect_images(
            &document,
            &document.get_page_content(page_id)?,
            &resources,
            &mut HashSet::new(),
            0,
            &mut sources,
        )
        .with_context(|| format!("Falha ao extrair as imagens da pagina {number}."))?;
        let mut images = Vec::with_capacity(sources.len());
        for source in sources {
            let png = extract_image(&document, &source.stream, &source.resources)?;
            anyhow::ensure!(
                png.len() <= MAX_IMAGE_BYTES,
                "Uma imagem do PDF excede o limite de 5 MB."
            );
            image_bytes += png.len();
            anyhow::ensure!(
                image_bytes <= MAX_CONTEXT_IMAGE_BYTES,
                "As imagens do PDF excedem 32 MB. Divida o documento em anexos menores. Nenhuma imagem foi omitida silenciosamente."
            );
            images.push(PdfImageContext {
                data: base64::engine::general_purpose::STANDARD.encode(png),
            });
        }
        pages.push(PdfPageContext {
            number,
            text,
            images,
        });
    }
    Ok(PdfContext { pages })
}

struct ImageSource {
    stream: Stream,
    resources: Dictionary,
}

fn page_resources(document: &Document, page_id: ObjectId) -> Result<Dictionary> {
    let mut node = document.get_dictionary(page_id)?;
    let mut seen = HashSet::from([page_id]);
    loop {
        if node.has(b"Resources") {
            return Ok(document
                .dereference(node.get(b"Resources")?)?
                .1
                .as_dict()?
                .clone());
        }
        if !node.has(b"Parent") {
            return Ok(Dictionary::new());
        }
        let parent = node.get(b"Parent")?.as_reference()?;
        anyhow::ensure!(
            seen.insert(parent),
            "O PDF possui uma arvore de paginas circular."
        );
        node = document.get_dictionary(parent)?;
    }
}

fn collect_images(
    document: &Document,
    content: &[u8],
    resources: &Dictionary,
    seen: &mut HashSet<ObjectId>,
    depth: usize,
    images: &mut Vec<ImageSource>,
) -> Result<()> {
    anyhow::ensure!(depth <= 32, "O PDF possui recursos aninhados em excesso.");
    for operation in Content::decode(content)?.operations {
        if operation.operator == "BI" {
            let Some(Object::Stream(stream)) = operation.operands.first() else {
                anyhow::bail!(
                    "Uma imagem inline usa um formato que o leitor ainda nao consegue extrair."
                );
            };
            let mut stream = stream.clone();
            for (short, full) in [
                ("W", "Width"),
                ("H", "Height"),
                ("BPC", "BitsPerComponent"),
                ("CS", "ColorSpace"),
                ("IM", "ImageMask"),
                ("D", "Decode"),
                ("I", "Interpolate"),
            ] {
                if let Some(value) = stream.dict.remove(short.as_bytes()) {
                    stream.dict.set(full, value);
                }
            }
            if let Ok(name) = stream.dict.get(b"ColorSpace").and_then(Object::as_name) {
                let normalized = match name {
                    b"RGB" => Some("DeviceRGB"),
                    b"G" | b"Gray" => Some("DeviceGray"),
                    b"CMYK" => Some("DeviceCMYK"),
                    _ => None,
                };
                if let Some(normalized) = normalized {
                    stream
                        .dict
                        .set("ColorSpace", Object::Name(normalized.as_bytes().to_vec()));
                }
            }
            stream.dict.set("Subtype", Object::Name(b"Image".to_vec()));
            images.push(ImageSource {
                stream,
                resources: resources.clone(),
            });
        } else if operation.operator == "Do" {
            let name = operation
                .operands
                .first()
                .context("XObject sem nome.")?
                .as_name()?;
            let (_, xobjects) = document.dereference(resources.get(b"XObject")?)?;
            let (id, object) = document.dereference(xobjects.as_dict()?.get(name)?)?;
            if let Some(id) = id
                && !seen.insert(id)
            {
                continue;
            }
            let stream = object.as_stream()?;
            match stream.dict.get(b"Subtype")?.as_name()? {
                b"Image" => images.push(ImageSource {
                    stream: stream.clone(),
                    resources: resources.clone(),
                }),
                b"Form" => {
                    let nested = if stream.dict.has(b"Resources") {
                        document
                            .dereference(stream.dict.get(b"Resources")?)?
                            .1
                            .as_dict()?
                    } else {
                        resources
                    };
                    collect_images(
                        document,
                        &stream.get_plain_content()?,
                        nested,
                        seen,
                        depth + 1,
                        images,
                    )?;
                }
                _ => {}
            }
        }
        anyhow::ensure!(
            images.len() <= 1000,
            "O PDF possui imagens demais por pagina."
        );
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn extract_image(
    _document: &Document,
    _image: &Stream,
    _resources: &Dictionary,
) -> Result<Vec<u8>> {
    anyhow::bail!("A extracao de imagens PDF ainda nao esta disponivel nesta plataforma.")
}

#[cfg(target_os = "windows")]
fn extract_image(document: &Document, image: &Stream, resources: &Dictionary) -> Result<Vec<u8>> {
    use windows::{
        Data::Pdf::{PdfDocument, PdfPageRenderOptions},
        Graphics::Imaging::BitmapEncoder,
        Storage::Streams::{DataReader, DataWriter, InMemoryRandomAccessStream},
        Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
    };
    let width = image.dict.get(b"Width")?.as_i64()?;
    let height = image.dict.get(b"Height")?.as_i64()?;
    anyhow::ensure!(
        width > 0 && height > 0 && width.saturating_mul(height) <= 25_000_000,
        "Uma imagem do PDF possui dimensoes invalidas ou excede 25 megapixels."
    );

    // Decode just the embedded image with the native PDF engine, retaining its color space and masks.
    // No source-page text or layout is rasterized.
    let mut isolated = document.clone();
    isolated.trailer = Dictionary::new();
    let image_id = isolated.add_object(image.clone());
    let content = isolated.add_object(Stream::new(
        Dictionary::new(),
        format!("q {width} 0 0 {height} 0 0 cm /Image Do Q").into_bytes(),
    ));
    let mut isolated_resources = dictionary! { "XObject" => dictionary! { "Image" => image_id } };
    if resources.has(b"ColorSpace") {
        isolated_resources.set("ColorSpace", resources.get(b"ColorSpace")?.clone());
    }
    let pages_id = isolated.new_object_id();
    let page_id = isolated.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), width.into(), height.into()],
        "Resources" => isolated_resources, "Contents" => content,
    });
    isolated.objects.insert(
        pages_id,
        dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
        }
        .into(),
    );
    let root = isolated.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    isolated.trailer.set("Root", root);
    isolated.prune_objects();
    let mut bytes = Vec::new();
    isolated.save_to(&mut bytes)?;

    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { RoUninitialize() };
        }
    }
    // All WinRT calls and their apartment remain on the same background worker.
    unsafe { RoInitialize(RO_INIT_MULTITHREADED) }?;
    let _apartment = Apartment;
    let stream = InMemoryRandomAccessStream::new()?;
    let writer = DataWriter::CreateDataWriter(&stream)?;
    writer.WriteBytes(&bytes)?;
    anyhow::ensure!(
        writer.StoreAsync()?.get()? as usize == bytes.len(),
        "Leitura incompleta da imagem PDF."
    );
    writer.DetachStream()?;
    stream.Seek(0)?;
    let document = PdfDocument::LoadFromStreamAsync(&stream)?.get()?;
    let page = document.GetPage(0)?;
    let options = PdfPageRenderOptions::new()?;
    options.SetBackgroundColor(windows::UI::Color {
        A: 0,
        R: 0,
        G: 0,
        B: 0,
    })?;
    let scale = (MAX_IMAGE_EDGE / width.max(height) as f32).min(1.0);
    options.SetDestinationWidth((width as f32 * scale).round().max(1.0) as u32)?;
    options.SetDestinationHeight((height as f32 * scale).round().max(1.0) as u32)?;
    options.SetBitmapEncoderId(BitmapEncoder::PngEncoderId()?)?;
    let output = InMemoryRandomAccessStream::new()?;
    page.RenderWithOptionsToStreamAsync(&output, &options)?
        .get()?;
    page.Close()?;
    let size = u32::try_from(output.Size()?)?;
    anyhow::ensure!(
        size > 0 && size as usize <= MAX_IMAGE_BYTES,
        "A imagem PDF excede 5 MB."
    );
    let reader = DataReader::CreateDataReader(&output.GetInputStreamAt(0)?)?;
    anyhow::ensure!(
        reader.LoadAsync(size)?.get()? == size,
        "Imagem PDF incompleta."
    );
    let mut png = vec![0; size as usize];
    reader.ReadBytes(&mut png)?;
    reader.DetachStream()?;
    output.Close()?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(with_image: bool, nested: bool) -> Result<Vec<u8>> {
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        });
        let mut resources = dictionary! { "Font" => dictionary! { "F1" => font_id } };
        let mut text = b"BT /F1 12 Tf 20 100 Td (Momor PDF text remains text) Tj ET".to_vec();
        if with_image {
            let image_id = document.add_object(Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Image", "Width" => 32, "Height" => 16,
                    "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
                },
                [255, 0, 0].repeat(32 * 16),
            ));
            let object_id = if nested {
                document.add_object(Stream::new(dictionary! {
                    "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 32.into(), 16.into()],
                    "Resources" => dictionary! { "XObject" => dictionary! { "NestedImage" => image_id } },
                }, b"q 32 0 0 16 0 0 cm /NestedImage Do Q".to_vec()))
            } else {
                image_id
            };
            resources.set("XObject", dictionary! { "Picture" => object_id });
            text.extend_from_slice(b" q 32 0 0 16 20 20 cm /Picture Do Q");
        }
        let content = document.add_object(Stream::new(Dictionary::new(), text));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
            "Contents" => content,
        });
        // Exercise inherited resources rather than only a direct page dictionary.
        document.objects.insert(pages_id, dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1, "Resources" => resources,
        }.into());
        let root = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        document.trailer.set("Root", root);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes)?;
        Ok(bytes)
    }

    #[test]
    fn text_only_pdf_does_not_create_images_or_require_vision() -> Result<()> {
        let context = extract_pdf_context(&fixture(false, false)?)?;
        assert_eq!(context.pages.len(), 1);
        assert!(
            context.pages[0]
                .text
                .contains("Momor PDF text remains text")
        );
        assert!(context.pages[0].images.is_empty());
        context.ensure_image_support(false)?;
        for embedded in [true, false] {
            assert!(
                context
                    .content_blocks("exam.pdf", "file:///exam.pdf", embedded, false)?
                    .iter()
                    .all(|block| !matches!(block, acp::ContentBlock::Image(_)))
            );
        }
        Ok(())
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn embedded_images_are_extracted_without_rasterizing_page_text() -> Result<()> {
        for nested in [false, true] {
            let context = extract_pdf_context(&fixture(true, nested)?)?;
            assert!(
                context.pages[0]
                    .text
                    .contains("Momor PDF text remains text")
            );
            assert_eq!(context.pages[0].images.len(), 1);
            let png = base64::engine::general_purpose::STANDARD
                .decode(&context.pages[0].images[0].data)?;
            let image = image::load_from_memory(&png)?.to_rgb8();
            assert_eq!((image.width(), image.height()), (32, 16));
            assert!(
                image
                    .pixels()
                    .all(|pixel| pixel[0] > 240 && pixel[1] < 15 && pixel[2] < 15)
            );
            assert!(context.ensure_image_support(false).is_err());
            for embedded in [true, false] {
                let blocks =
                    context.content_blocks("exam.pdf", "file:///exam.pdf", embedded, true)?;
                assert_eq!(
                    blocks
                        .iter()
                        .filter(|block| matches!(block, acp::ContentBlock::Image(_)))
                        .count(),
                    1
                );
                assert!(serde_json::to_string(&blocks)?.contains("Momor PDF text remains text"));
            }
        }
        Ok(())
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn inline_images_remain_separate_from_text() -> Result<()> {
        let mut document = Document::load_mem(&fixture(false, false)?)?;
        let page_id = *document.get_pages().get(&1).context("test page")?;
        document.add_page_contents(
            page_id,
            b"q BI /W 1 /H 1 /CS /RGB /BPC 8 ID \xff\x00\x00 EI Q".to_vec(),
        )?;
        let mut bytes = Vec::new();
        document.save_to(&mut bytes)?;
        let context = extract_pdf_context(&bytes)?;
        assert!(
            context.pages[0]
                .text
                .contains("Momor PDF text remains text")
        );
        assert_eq!(context.pages[0].images.len(), 1);
        let png =
            base64::engine::general_purpose::STANDARD.decode(&context.pages[0].images[0].data)?;
        let image = image::load_from_memory(&png)?.to_rgb8();
        assert_eq!((image.width(), image.height()), (1, 1));
        assert_eq!(image.get_pixel(0, 0).0, [255, 0, 0]);
        Ok(())
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn scans_keep_their_original_image_and_do_not_invent_text() -> Result<()> {
        let mut document = Document::load_mem(&fixture(true, false)?)?;
        let page_id = *document.get_pages().get(&1).context("test page")?;
        let content_id = document
            .get_dictionary(page_id)?
            .get(b"Contents")?
            .as_reference()?;
        let stream = document.get_object_mut(content_id)?.as_stream_mut()?;
        stream.set_plain_content(b"q 32 0 0 16 20 20 cm /Picture Do Q".to_vec());
        let mut bytes = Vec::new();
        document.save_to(&mut bytes)?;
        let context = extract_pdf_context(&bytes)?;
        assert!(context.pages[0].text.trim().is_empty());
        assert_eq!(context.pages[0].images.len(), 1);
        Ok(())
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn compressed_images_and_soft_masks_keep_their_colors_and_alpha() -> Result<()> {
        let mut document = Document::load_mem(&fixture(true, false)?)?;
        let image_id = document
            .objects
            .iter()
            .find_map(|(id, object)| {
                object
                    .as_stream()
                    .ok()
                    .filter(|stream| {
                        stream.dict.get(b"Subtype").and_then(Object::as_name).ok()
                            == Some(b"Image".as_slice())
                    })
                    .map(|_| *id)
            })
            .context("test image")?;
        let mask_id = document.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image", "Width" => 32, "Height" => 16,
                "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8,
            },
            vec![128; 32 * 16],
        ));
        let stream = document.get_object_mut(image_id)?.as_stream_mut()?;
        stream.dict.set("SMask", mask_id);
        stream.compress()?;
        let mut bytes = Vec::new();
        document.save_to(&mut bytes)?;
        let context = extract_pdf_context(&bytes)?;
        assert_eq!(
            context.pages[0].images.len(),
            1,
            "a transparency mask is not a separate figure"
        );
        let png =
            base64::engine::general_purpose::STANDARD.decode(&context.pages[0].images[0].data)?;
        let image = image::load_from_memory(&png)?.to_rgba8();
        let pixel = image.get_pixel(16, 8).0;
        assert!(pixel[0] > 240 && pixel[1] < 15 && pixel[2] < 15);
        assert!(
            (120..=136).contains(&pixel[3]),
            "the image must retain its transparency: {pixel:?}"
        );
        Ok(())
    }

    #[test]
    fn text_and_multiple_images_keep_page_order_in_both_transport_modes() -> Result<()> {
        let context = PdfContext {
            pages: vec![
                PdfPageContext {
                    number: 1,
                    text: "Question 1: y = 3x / 2".into(),
                    images: vec![
                        PdfImageContext {
                            data: "figure-one".into(),
                        },
                        PdfImageContext {
                            data: "figure-two".into(),
                        },
                    ],
                },
                PdfPageContext {
                    number: 2,
                    text: "Question 2: probability = 1 - 0.05^4".into(),
                    images: vec![PdfImageContext {
                        data: "figure-three".into(),
                    }],
                },
            ],
        };
        for embedded in [true, false] {
            let blocks = context.content_blocks("exam.pdf", "file:///exam.pdf", embedded, true)?;
            let images = blocks
                .iter()
                .filter_map(|block| match block {
                    acp::ContentBlock::Image(image) => {
                        Some((image.data.as_str(), image.uri.as_deref()))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                images,
                vec![
                    ("figure-one", Some("file:///exam.pdf#page=1&image=1")),
                    ("figure-two", Some("file:///exam.pdf#page=1&image=2")),
                    ("figure-three", Some("file:///exam.pdf#page=2&image=1")),
                ]
            );
            let json = serde_json::to_string(&blocks)?;
            assert!(json.contains("y = 3x / 2"));
            assert!(json.contains("1 - 0.05^4"));
        }
        Ok(())
    }

    #[test]
    fn reports_invalid_pdf_and_recognizes_uppercase_extension() {
        assert!(is_pdf_path(Path::new("attachment.PDF")));
        assert!(!is_pdf_path(Path::new("attachment.txt")));
        assert!(extract_pdf_context(b"not a PDF").is_err());
    }
}
