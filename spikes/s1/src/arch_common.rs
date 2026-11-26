//! Types shared by the per-architecture modules.

/// Offsets of the generated-code blob's labels, relative to its start.
pub struct BlobLayout {
    pub len: usize,
    pub null_load: usize,
    /// The load that performs the implicit null check.
    pub null_site: usize,
    pub null_load_after: usize,
    pub npe_stub: usize,
    pub so_entry: usize,
    pub wso_entry: usize,
    pub so_stub: usize,
    pub unw_func: usize,
    pub unw_prolog_end: usize,
    pub unw_after: usize,
    pub unw_func_end: usize,
    pub slot_npe: usize,
    pub slot_sp: usize,
    pub slot_entry_sp: usize,
}
