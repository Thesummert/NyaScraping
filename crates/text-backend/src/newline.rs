use nya_domain::LineEnding;

pub(crate) fn normalize(text: &str) -> (String, LineEnding, bool) {
    let mut normalized = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    let (mut lf, mut crlf, mut cr) = (0, 0, 0);
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                    crlf += 1;
                } else {
                    cr += 1;
                }
                normalized.push('\n');
            }
            '\n' => {
                lf += 1;
                normalized.push('\n');
            }
            other => normalized.push(other),
        }
    }
    let ending = if crlf > 0 && crlf >= lf && crlf >= cr {
        LineEnding::CrLf
    } else if cr > lf {
        LineEnding::Cr
    } else {
        LineEnding::Lf
    };
    let mixed = usize::from(lf > 0) + usize::from(crlf > 0) + usize::from(cr > 0) > 1;
    (normalized, ending, mixed)
}

pub(crate) fn render(text: &str, ending: LineEnding) -> String {
    let (text, _, _) = normalize(text);
    match ending {
        LineEnding::Lf => text,
        LineEnding::CrLf => text.replace('\n', "\r\n"),
        LineEnding::Cr => text.replace('\n', "\r"),
    }
}
