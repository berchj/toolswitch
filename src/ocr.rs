// Text from screen: reads the text in part of a screenshot with ocrs, offline.
// The two models (~12 MB) are downloaded the first time it's used.
// ocrs only knows letters without accents: "canción" comes out as "cancion".

use ocrs::{ImageSource, OcrEngine, OcrEngineParams};
use std::sync::OnceLock;

const MODELS_URL: &str = "https://ocrs-models.s3-accelerate.amazonaws.com";
const DETECTION: &str = "text-detection.onnx";
const RECOGNITION: &str = "text-recognition.onnx";

fn model(name: &str) -> Result<rten::Model, String> {
    crate::models::load(name, &format!("{MODELS_URL}/{name}"))
}

/// Loading the models takes a moment, so it happens once.
fn engine() -> Result<&'static OcrEngine, String> {
    static ENGINE: OnceLock<Result<OcrEngine, String>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            OcrEngine::new(OcrEngineParams {
                detection_model: Some(model(DETECTION)?),
                recognition_model: Some(model(RECOGNITION)?),
                ..Default::default()
            })
            .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// The text in the image, one line per line of text.
pub fn read(image: &image::RgbImage) -> Result<String, String> {
    let engine = engine()?;
    let source =
        ImageSource::from_bytes(image.as_raw(), image.dimensions()).map_err(|e| e.to_string())?;
    let input = engine.prepare_input(source).map_err(|e| e.to_string())?;
    let text = engine.get_text(&input).map_err(|e| e.to_string())?;
    Ok(fix_accents(text.trim()))
}

// ---------- accents ----------

// ocrs doesn't know á, é, í, ó, ú, ñ: it reads them as "?". Common Spanish words and
// endings get their letter back; anything else keeps the "?".
const WORDS: &[(&str, &str)] = &[
    ("qu?", "qué"),
    ("est?", "está"),
    ("est?n", "están"),
    ("est?s", "estás"),
    ("m?s", "más"),
    ("tambi?n", "también"),
    ("despu?s", "después"),
    ("as?", "así"),
    ("aqu?", "aquí"),
    ("ah?", "ahí"),
    ("all?", "allí"),
    ("ac?", "acá"),
    ("s?", "sí"),
    ("t?", "tú"),
    ("?l", "él"),
    ("a?n", "aún"),
    ("s?lo", "solo"),
    ("d?a", "día"),
    ("d?as", "días"),
    ("a?o", "año"),
    ("a?os", "años"),
    ("ni?o", "niño"),
    ("ni?os", "niños"),
    ("ni?a", "niña"),
    ("ma?ana", "mañana"),
    ("espa?ol", "español"),
    ("espa?a", "españa"),
    ("dise?o", "diseño"),
    ("peque?o", "pequeño"),
    ("peque?a", "pequeña"),
    ("contrase?a", "contraseña"),
    ("se?or", "señor"),
    ("se?ora", "señora"),
    ("p?gina", "página"),
    ("p?ginas", "páginas"),
    ("n?mero", "número"),
    ("n?meros", "números"),
    ("m?vil", "móvil"),
    ("f?cil", "fácil"),
    ("dif?cil", "difícil"),
    ("r?pido", "rápido"),
    ("r?pida", "rápida"),
    ("r?pidos", "rápidos"),
    ("r?pidas", "rápidas"),
    ("autom?tico", "automático"),
    ("autom?tica", "automática"),
    ("autom?ticos", "automáticos"),
    ("autom?ticamente", "automáticamente"),
    ("pol?tica", "política"),
    ("pr?ctica", "práctica"),
    ("m?todo", "método"),
    ("cr?tica", "crítica"),
    ("t?tulo", "título"),
    ("m?ximo", "máximo"),
    ("m?nimo", "mínimo"),
    ("c?digo", "código"),
    ("aplicaci?n", "aplicación"),
    ("v?deo", "vídeo"),
    ("lim?n", "limón"),
    ("tel?fono", "teléfono"),
    ("cr?dito", "crédito"),
    ("d?bito", "débito"),
    ("pa?s", "país"),
    ("d?nde", "dónde"),
    ("c?mo", "cómo"),
    ("cu?ndo", "cuándo"),
    ("cu?l", "cuál"),
    ("qui?n", "quién"),
    ("cu?nto", "cuánto"),
    ("adem?s", "además"),
    ("seg?n", "según"),
    ("ser?", "será"),
    ("har?", "hará"),
    ("podr?", "podrá"),
    ("tendr?", "tendrá"),
    ("men?", "menú"),
    ("art?culo", "artículo"),
    ("p?blico", "público"),
    ("m?sica", "música"),
    ("pr?ximo", "próximo"),
    ("?ltimo", "último"),
    ("?ltima", "última"),
    ("sesi?n", "sesión"),
    ("raz?n", "razón"),
    ("coraz?n", "corazón"),
    ("bot?n", "botón"),
    ("im?genes", "imágenes"),
    ("ingl?s", "inglés"),
    ("caf?", "café"),
    ("mam?", "mamá"),
    ("pap?", "papá"),
    ("compa??a", "compañía"),
    ("tama?o", "tamaño"),
    ("due?o", "dueño"),
    ("se?al", "señal"),
    ("enga?o", "engaño"),
    ("ba?o", "baño"),
    ("oto?o", "otoño"),
];

pub fn fix_accents(text: &str) -> String {
    text.lines()
        .map(|line| line.split(' ').map(fix_word).collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

fn fix_word(word: &str) -> String {
    if !word.contains('?') {
        return word.to_string();
    }
    // Punctuation around the word stays as it is; a "?" at the end may be a real question mark.
    let start = word
        .find(|c: char| c.is_alphanumeric() || c == '?')
        .unwrap_or(0);
    let (lead, rest) = word.split_at(start);
    // Where the last letter ends: "ma?ana?" is the word "ma?ana" and a question mark.
    let end = rest
        .char_indices()
        .rfind(|(_, c)| c.is_alphanumeric())
        .map_or(0, |(i, c)| i + c.len_utf8());
    let (core, tail) = rest.split_at(end);
    if let Some(fixed) = lookup(rest) {
        return format!("{lead}{fixed}");
    }
    if let Some(fixed) = lookup(core).or_else(|| by_ending(core)) {
        return format!("{lead}{fixed}{tail}");
    }
    word.to_string()
}

/// From the word list, keeping a capital first letter.
fn lookup(word: &str) -> Option<String> {
    let lower = word.to_lowercase();
    let fixed = WORDS.iter().find(|(w, _)| *w == lower)?.1;
    Some(if word.starts_with(char::is_uppercase) {
        let mut chars = fixed.chars();
        chars
            .next()
            .map(|c| c.to_uppercase().chain(chars).collect())
            .unwrap_or_default()
    } else {
        fixed.to_string()
    })
}

/// Endings that only go one way: canci?n, decisi?n, ten?a, ser?an.
fn by_ending(word: &str) -> Option<String> {
    let lower = word.to_lowercase();
    for (ending, fixed) in [
        ("ci?n", "ción"),
        ("si?n", "sión"),
        ("xi?n", "xión"),
        ("ci?nes", "ciones"),
        ("?a", "ía"),
        ("?as", "ías"),
        ("?amos", "íamos"),
        ("?an", "ían"),
    ] {
        if lower.ends_with(ending) && lower.matches('?').count() == 1 {
            let stem = &word[..word.len() - ending.len()];
            // "?a" alone would match anything: the letter before has to be a consonant (ten?a).
            if ending.starts_with('?')
                && !stem.ends_with(|c: char| c.is_alphabetic() && !"aeiouAEIOU".contains(c))
            {
                return None;
            }
            return Some(format!("{stem}{fixed}"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn accents() {
        let fix = super::fix_accents;
        assert_eq!(fix("est? en marcha"), "está en marcha");
        assert_eq!(fix("La canci?n y la decisi?n"), "La canción y la decisión");
        assert_eq!(fix("Qu? pasa? ten?a raz?n."), "Qué pasa? tenía razón.");
        assert_eq!(fix("¿Vienes ma?ana?"), "¿Vienes mañana?");
        assert_eq!(fix("Hola?"), "Hola?");
        assert_eq!(fix("x?y"), "x?y");
    }

    // OCR_TEST_IMAGE=photo.png cargo test --release ocr_image -- --ignored --nocapture
    #[test]
    #[ignore]
    fn ocr_image() {
        let path = std::env::var("OCR_TEST_IMAGE").expect("set OCR_TEST_IMAGE");
        let image = image::open(path).unwrap().into_rgb8();
        let start = std::time::Instant::now();
        println!("{}", super::read(&image).unwrap());
        println!("({:?})", start.elapsed());
    }
}
