//! The pieces the conformance tools' command lines share (`conformance`,
//! the binaries in `src/bin`, pikapro-server's `stream_fuzz`): reading
//! flags, the flags most of them take, running seeds on worker threads,
//! and writing a failure's file.

use std::path::Path;

use pikapro::Format;

/// Worker threads by default: the cores, at most 4, because each oracle
/// run starts its own Node process and its memory; `--jobs` asks for more.
pub fn default_jobs() -> u64 {
    std::thread::available_parallelism().map_or(1, |n| n.get() as u64).min(4)
}

/// A command line, read flag by flag. A bad or missing value panics with
/// the flag's name (these are developer tools).
#[derive(Debug)]
pub struct Args {
    it: std::vec::IntoIter<String>,
}

impl Args {
    /// The process's arguments, without the program name.
    pub fn from_env() -> Args {
        Args::new(std::env::args().skip(1).collect())
    }

    pub fn new(args: Vec<String>) -> Args {
        Args { it: args.into_iter() }
    }

    /// The next flag (or bare argument).
    pub fn next_flag(&mut self) -> Option<String> {
        self.it.next()
    }

    /// The value after `flag`.
    pub fn value(&mut self, flag: &str) -> String {
        self.it.next().unwrap_or_else(|| panic!("{flag} needs a value"))
    }

    /// The value after a flag, or "" at the end (for a parser that reads
    /// the value before it knows the flag; its match reports either).
    pub fn value_or_empty(&mut self) -> String {
        self.it.next().unwrap_or_default()
    }

    /// The number after `flag`.
    pub fn num(&mut self, flag: &str) -> u64 {
        let v = self.value(flag);
        v.parse().unwrap_or_else(|_| panic!("{flag}: bad number {v:?}"))
    }

    /// The format preset named after `flag`.
    pub fn format(&mut self, flag: &str) -> Format {
        let v = self.value(flag);
        Format::by_id(&v).unwrap_or_else(|| panic!("unknown format {v:?}"))
    }
}

/// The flags most tools take: `--format`, `--battles`, `--seed`, `--jobs`
/// and `--show`.
#[derive(Debug)]
pub struct Common {
    pub format: Format,
    pub battles: u64,
    pub seed: u64,
    pub jobs: u64,
    /// Failures printed in full.
    pub show: usize,
}

impl Common {
    /// The defaults, with [`default_jobs`].
    pub fn new(format: Format, battles: u64, seed: u64, show: usize) -> Common {
        Common { format, battles, seed, jobs: default_jobs(), show }
    }

    /// Reads `flag` and its value if it is one of these: whether it was.
    pub fn take(&mut self, flag: &str, args: &mut Args) -> bool {
        match flag {
            "--format" => self.format = args.format(flag),
            "--battles" => self.battles = args.num(flag),
            "--seed" => self.seed = args.num(flag),
            "--jobs" => self.jobs = args.num(flag).max(1),
            "--show" => self.show = args.num(flag) as usize,
            _ => return false,
        }
        true
    }
}

/// `f(i)` for every `i` in `0..n`, on `jobs` threads (thread `j` takes the
/// `i` with `i % jobs == j`), in the order of `i`.
pub fn par_seeds<T: Send>(jobs: u64, n: u64, f: impl Fn(u64) -> T + Sync) -> Vec<T> {
    let jobs = jobs.max(1);
    let f = &f;
    let mut all: Vec<(u64, T)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..jobs)
            .map(|j| scope.spawn(move || (0..n).filter(|i| i % jobs == j).map(|i| (i, f(i))).collect::<Vec<_>>()))
            .collect();
        handles.into_iter().flat_map(|h| h.join().expect("worker panicked")).collect()
    });
    all.sort_by_key(|(i, _)| *i);
    all.into_iter().map(|(_, t)| t).collect()
}

/// Writes a failure's file; if it can't, says so on stderr and returns
/// false (the report then doesn't point at a file that isn't there).
pub fn save(path: &Path, text: impl AsRef<[u8]>) -> bool {
    match std::fs::write(path, text) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("can't write {}: {e}", path.display());
            false
        }
    }
}
