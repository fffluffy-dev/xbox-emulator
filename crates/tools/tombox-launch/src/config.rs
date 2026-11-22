#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GameConfig {
    pub identity: String,
    pub version: String,
    pub executables: Vec<Executable>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Executable {
    pub name: String,
    pub id: String,
    pub device_family: String,
}

fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("<!--") {
        out.push_str(&rest[..i]);
        match rest[i..].find("-->") {
            Some(j) => rest = &rest[i + j + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let mut search = tag;
    while let Some(i) = search.find(name) {
        let before = search[..i].chars().last();
        let after = &search[i + name.len()..];
        let after_trim = after.trim_start();
        if before.is_none_or(|c| c.is_whitespace()) && after_trim.starts_with('=') {
            let v = after_trim[1..].trim_start();
            let q = v.chars().next()?;
            if q == '"' || q == '\'' {
                let end = v[1..].find(q)?;
                return Some(v[1..1 + end].to_string());
            }
        }
        search = &search[i + name.len()..];
    }
    None
}

fn tags<'a>(s: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name}");
    let mut v = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find(&open) {
        let t = &rest[i + open.len()..];
        if t.starts_with(|c: char| c.is_whitespace() || c == '>' || c == '/') {
            let end = t.find('>').unwrap_or(t.len());
            v.push(&t[..end]);
        }
        rest = &rest[i + open.len()..];
    }
    v
}

pub fn parse(text: &str) -> GameConfig {
    let s = strip_comments(text);
    let mut c = GameConfig::default();
    if let Some(t) = tags(&s, "Identity").first() {
        c.identity = attr(t, "Name").unwrap_or_default();
        c.version = attr(t, "Version").unwrap_or_default();
    }
    for t in tags(&s, "Executable") {
        if let Some(name) = attr(t, "Name") {
            c.executables.push(Executable { name, id: attr(t, "Id").unwrap_or_default(), device_family: attr(t, "TargetDeviceFamily").unwrap_or_default() });
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_identity_and_executables_and_skips_commented_entries() {
        let x = r#"<?xml version="1.0"?>
<Game configVersion="1">
  <Identity Name="Ubisoft.Sample" Publisher="CN=X" Version="1.6.2.0"/>
  <ExecutableList>
    <Executable Name="GbxGame.exe" Id="Game" TargetDeviceFamily="XboxOne"/>
    <!-- <Executable Name="Other.exe" Id="Tool" TargetDeviceFamily="PC"/> -->
  </ExecutableList>
</Game>"#;
        let c = parse(x);
        assert_eq!(c.identity, "Ubisoft.Sample");
        assert_eq!(c.version, "1.6.2.0");
        assert_eq!(c.executables, vec![Executable { name: "GbxGame.exe".into(), id: "Game".into(), device_family: "XboxOne".into() }]);
    }

    #[test]
    fn executable_list_tag_is_not_an_executable() {
        let c = parse(r#"<ExecutableList><Executable Name='a.exe' Id='Game'/></ExecutableList>"#);
        assert_eq!(c.executables.len(), 1);
        assert_eq!(c.executables[0].device_family, "");
    }
}
