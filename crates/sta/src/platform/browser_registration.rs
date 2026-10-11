//! Registry contract shared by the Windows registration code and its tests.
//! Portable copies have their own ProgID so they cannot shadow an MSI installation.

pub struct Entry {
    pub key: String,
    pub name: String,
    pub value: String,
}

pub fn portable_entries(exe: &str) -> Vec<Entry> {
    let client = r"Software\Clients\StartMenuInternet\staPortable";
    let caps = format!(r"{client}\Capabilities");
    let class = r"Software\Classes\staPortableHTML";
    let icon = format!("\"{exe}\",0");
    let mut entries = Vec::new();
    let mut add = |key: &str, name: &str, value: &str| {
        entries.push(Entry { key: key.into(), name: name.into(), value: value.into() });
    };
    add(class, "", "sta HTML Document");
    add(class, "URL Protocol", "");
    add(&format!(r"{class}\DefaultIcon"), "", &icon);
    add(&format!(r"{class}\shell\open\command"), "", &format!("\"{exe}\" -- \"%1\""));
    add(client, "", "sta Portable");
    add(&format!(r"{client}\DefaultIcon"), "", &icon);
    add(&format!(r"{client}\shell\open\command"), "", &format!("\"{exe}\""));
    add(&caps, "ApplicationName", "sta Portable");
    add(&caps, "ApplicationDescription", "Browse the web with sta");
    add(&caps, "ApplicationIcon", &icon);
    for scheme in ["http", "https"] {
        add(&format!(r"{caps}\URLAssociations"), scheme, "staPortableHTML");
    }
    for extension in [".htm", ".html"] {
        add(&format!(r"{caps}\FileAssociations"), extension, "staPortableHTML");
    }
    // Publish last: a failed write must not advertise an incomplete new registration.
    add(r"Software\RegisteredApplications", "sta Portable", &caps);
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_handlers_resolve_to_the_quoted_executable() {
        let entries = portable_entries(r"C:\Users\테스트 User\sta\sta.exe");
        let get = |key: &str, name: &str| entries.iter().find(|e| e.key == key && e.name == name).map(|e| e.value.as_str());
        let caps = get(r"Software\RegisteredApplications", "sta Portable").expect("registered application");
        for (kind, associations) in [("URLAssociations", vec!["http", "https"]), ("FileAssociations", vec![".htm", ".html"])] {
            for association in associations {
                let id = get(&format!(r"{caps}\{kind}"), association).expect("association");
                assert_ne!(id, "staHTML", "portable registration must not shadow MSI");
                assert_eq!(get(&format!(r"Software\Classes\{id}\shell\open\command"), ""), Some("\"C:\\Users\\테스트 User\\sta\\sta.exe\" -- \"%1\""));
            }
        }
        assert!(entries.iter().all(|e| !e.key.contains("UserChoice")), "registration never changes defaults");
        assert_eq!(get(r"Software\Classes\staPortableHTML\DefaultIcon", ""), Some("\"C:\\Users\\테스트 User\\sta\\sta.exe\",0"));
    }
}
