//! Minimal `--key value` argument parsing; everything after `--` is passed
//! through.

pub struct Args {
    pub cmd: String,
    rest: Vec<String>,
    pub tail: Vec<String>,
}

impl Args {
    pub fn from_env() -> Args {
        let mut v: Vec<String> = std::env::args().skip(1).collect();
        let cmd = if v.is_empty() {
            String::new()
        } else {
            v.remove(0)
        };
        let (rest, tail) = match v.iter().position(|a| a == "--") {
            Some(i) => (v[..i].to_vec(), v[i + 1..].to_vec()),
            None => (v, vec![]),
        };
        Args { cmd, rest, tail }
    }

    fn get(&self, k: &str) -> Option<&str> {
        let i = self.rest.iter().position(|a| a == k)?;
        self.rest.get(i + 1).map(|s| s.as_str())
    }

    pub fn flag(&self, k: &str) -> bool {
        self.rest.iter().any(|a| a == k)
    }

    pub fn opt(&self, k: &str) -> Option<String> {
        self.get(k).map(str::to_string)
    }

    pub fn str(&self, k: &str, d: &str) -> String {
        self.get(k).unwrap_or(d).to_string()
    }

    pub fn num<T: std::str::FromStr>(&self, k: &str, d: T) -> T {
        self.get(k).and_then(|v| v.parse().ok()).unwrap_or(d)
    }
}
