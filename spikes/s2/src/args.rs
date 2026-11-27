//! Minimal `--key value` argument parsing.

pub struct Args {
    pub cmd: String,
    rest: Vec<String>,
}

impl Args {
    pub fn from_env() -> Args {
        let mut v: Vec<String> = std::env::args().skip(1).collect();
        let cmd = if v.is_empty() {
            String::new()
        } else {
            v.remove(0)
        };
        Args { cmd, rest: v }
    }

    fn get(&self, k: &str) -> Option<&str> {
        let i = self.rest.iter().position(|a| a == k)?;
        self.rest.get(i + 1).map(|s| s.as_str())
    }

    pub fn flag(&self, k: &str) -> bool {
        self.rest.iter().any(|a| a == k)
    }

    pub fn str(&self, k: &str, d: &str) -> String {
        self.get(k).unwrap_or(d).to_string()
    }

    pub fn num<T: std::str::FromStr>(&self, k: &str, d: T) -> T {
        self.get(k).and_then(|v| v.parse().ok()).unwrap_or(d)
    }

    pub fn hex(&self, k: &str, d: usize) -> usize {
        self.get(k)
            .and_then(|v| usize::from_str_radix(v.trim_start_matches("0x"), 16).ok())
            .unwrap_or(d)
    }

    pub fn list(&self, k: &str, d: &[usize]) -> Vec<usize> {
        self.get(k)
            .map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect())
            .unwrap_or_else(|| d.to_vec())
    }
}
