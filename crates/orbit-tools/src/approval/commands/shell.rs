//! A deliberately limited shell grammar, not a general script safety classifier.
//! Parse and rebuild literal stages; paths are checked separately before Auto approval.
use orbit_runtime::execution::process::system_executable;

pub(super) struct Plan {
    pub script: String,
    pub paths: Vec<String>,
}

#[derive(Debug)]
enum Token {
    Word(String),
    Pipe,
    And,
    Output { append: bool },
}

pub(super) fn plan(source: &str) -> Option<Plan> {
    let tokens = tokenize(source)?;
    let mut tokens = tokens.into_iter().peekable();
    let mut plan = Plan {
        script: String::new(),
        paths: Vec::new(),
    };

    loop {
        let mut words = Vec::new();
        while matches!(tokens.peek(), Some(Token::Word(_))) {
            if let Some(Token::Word(word)) = tokens.next() {
                words.push(word);
            }
        }

        let stage = program(&mut words, &mut plan.paths)?;
        plan.script.push_str(&stage);

        if matches!(tokens.peek(), Some(Token::Output { .. })) {
            let Token::Output { append } = tokens.next()? else {
                unreachable!()
            };
            let Token::Word(path) = tokens.next()? else {
                return None;
            };
            if path.is_empty() {
                return None;
            }
            plan.script.push_str(if append { " >> " } else { " > " });
            plan.script.push_str(&shlex::try_quote(&path).ok()?);
            plan.paths.push(path);
        }

        match tokens.next() {
            Some(Token::Pipe) => plan.script.push_str(" | "),
            Some(Token::And) => plan.script.push_str(" && "),
            None => return Some(plan),
            _ => return None,
        }
    }
}

fn program(words: &mut Vec<String>, paths: &mut Vec<String>) -> Option<String> {
    let name = words.first()?.rsplit('/').next()?.to_owned();
    if words[0] != name
        && words[0] != format!("/bin/{name}")
        && words[0] != format!("/usr/bin/{name}")
    {
        return None;
    }

    match name.as_str() {
        "echo" | "printf" => {
            // POSIX command bypasses shell functions; keep the same /bin/sh
            // builtins so echo/printf retain their original escape semantics.
            if words[0] == name {
                words.insert(0, "command".into());
            } else {
                words[0] = system_executable(&name)?.to_str()?.to_owned();
            }
        }

        "mkdir" | "cat" | "tee" => {
            let mut options = true;
            let mut count = 0;
            for word in &words[1..] {
                if options && word == "--" {
                    options = false;
                    continue;
                }

                if options && word.starts_with('-') && word != "-" {
                    if !allowed_option(&name, word) {
                        return None;
                    }
                    continue;
                }

                if word.is_empty() {
                    return None;
                }
                // '-' denotes standard input for cat, but is a filename for tee.
                if name != "cat" || word != "-" {
                    paths.push(word.clone());
                }
                count += 1;
            }

            if name == "mkdir" && count == 0 {
                return None;
            }
            words[0] = system_executable(&name)?.to_str()?.to_owned();
        }

        _ => return None,
    }

    shlex::try_join(words.iter().map(String::as_str)).ok()
}

fn allowed_option(program: &str, word: &str) -> bool {
    let (long, short): (&[&str], &str) = match program {
        "mkdir" => (&["--parents", "--verbose"], "pv"),
        "cat" => (
            &[
                "--number",
                "--number-nonblank",
                "--squeeze-blank",
                "--show-ends",
                "--show-tabs",
                "--show-nonprinting",
                "--show-all",
            ],
            "nbsvETAetu",
        ),

        "tee" => (&["--append"], "a"),
        _ => return false,
    };
    long.contains(&word)
        || word.strip_prefix('-').is_some_and(|flags| {
            !flags.is_empty() && flags.chars().all(|flag| short.contains(flag))
        })
}

fn tokenize(source: &str) -> Option<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut chars = source.char_indices().peekable();
    let mut quote = None;
    let mut start = 0;
    while let Some((index, ch)) = chars.next() {
        match (quote, ch) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), _) => {}
            (_, '\\') => {
                chars.next()?;
            }
            (Some('"'), _) => {}
            (None, '\'' | '"') => quote = Some(ch),
            (None, '|' | '&' | '>') => {
                let part = &source[start..index];
                // File-descriptor redirects need a broader grammar; do not
                // reinterpret an adjacent IO number as an echo argument.
                if ch == '>'
                    && part.ends_with(|ch: char| ch.is_ascii_digit())
                    && part
                        .split_whitespace()
                        .next_back()
                        .is_some_and(|word| word.bytes().all(|b| b.is_ascii_digit()))
                {
                    return None;
                }

                tokens.extend(super::literal_words(part)?.into_iter().map(Token::Word));
                let next = chars.peek().map(|(_, ch)| *ch);
                let token = match ch {
                    '|' if next != Some('|') => Token::Pipe,
                    '&' if next == Some('&') => {
                        chars.next();
                        Token::And
                    }

                    '>' => {
                        let append = next == Some('>');
                        if append {
                            chars.next();
                        }
                        Token::Output { append }
                    }

                    _ => return None,
                };

                tokens.push(token);
                start = chars.peek().map_or(source.len(), |(index, _)| *index);
            }

            _ => {}
        }
    }

    tokens.extend(
        super::literal_words(&source[start..])?
            .into_iter()
            .map(Token::Word),
    );
    Some(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_handles_adjacent_operators_and_keeps_quoted_punctuation_literal() {
        let parsed = plan("mkdir -p demo&&printf '%s\\n' '$(literal) | data > text'|tee -a 'demo/a b' >demo/output").unwrap();
        assert_eq!(parsed.paths, vec!["demo", "demo/a b", "demo/output"]);
        assert!(parsed.script.contains("command printf"));
        assert!(
            parsed
                .script
                .contains(system_executable("tee").unwrap().to_str().unwrap())
        );
        assert!(parsed.script.contains("'$(literal) | data > text'"));
        assert!(plan("echo 2 > demo/text").is_some());
        assert!(plan("echo '2'>demo/text").is_some());
    }

    #[test]
    fn parser_rejects_evaluation_unknown_programs_and_ambiguous_redirections() {
        for source in [
            "echo $VALUE > demo/a",
            "echo \"$(touch file)\" > demo/a",
            "echo hi > $FILE",
            "echo hi | sh",
            "echo hi | tee file; rm other",
            "echo hi || printf failed",
            "echo hi &",
            "echo hi\necho bye",
            "echo hi 2>demo/errors",
            "echo hi >file extra",
            "echo hi >file >other",
            "echo hi >&2",
            "echo hi >>>file",
            "echo hi >file |",
            "echo hi &&",
            "cat < file >out",
            "cat `pwd`/file",
            "tee --output-error=exit file",
            "mkdir -m 777 demo",
            "./echo hi >file",
            "env echo hi >file",
        ] {
            assert!(plan(source).is_none(), "{source}");
        }
    }
}
