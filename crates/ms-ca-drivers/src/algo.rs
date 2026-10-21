//! Descrambling algorithms and the ordered op registry (`register_algo`, `get_op_for_algo`).

/// Descrambling algorithm of a control word, with the C++ `CA_ALGO_*` values (pmt.h).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Algo {
    /// `CA_ALGO_DVBCSA`.
    #[default]
    Csa,
    /// `CA_ALGO_DES`.
    Des,
    /// `CA_ALGO_AES128_ECB`.
    Aes128Ecb,
    /// `CA_ALGO_AES128_CBC`.
    Aes128Cbc,
    /// `CA_ALGO_DVBCSA_ICAM`.
    CsaIcam,
}

impl Algo {
    /// The `CA_ALGO_*` number.
    pub const fn raw(self) -> u32 {
        match self {
            Self::Csa => 0,
            Self::Des => 1,
            Self::Aes128Ecb => 2,
            Self::Aes128Cbc => 3,
            Self::CsaIcam => 4,
        }
    }

    /// Parses a `CA_ALGO_*` number.
    pub const fn from_raw(raw: u32) -> Option<Self> {
        Some(match raw {
            0 => Self::Csa,
            1 => Self::Des,
            2 => Self::Aes128Ecb,
            3 => Self::Aes128Cbc,
            4 => Self::CsaIcam,
            _ => return None,
        })
    }
}

impl From<ms_ca::CwAlgo> for Algo {
    fn from(algo: ms_ca::CwAlgo) -> Self {
        match algo {
            ms_ca::CwAlgo::Csa => Self::Csa,
            ms_ca::CwAlgo::Des => Self::Des,
            ms_ca::CwAlgo::Aes128Ecb => Self::Aes128Ecb,
            ms_ca::CwAlgo::Aes128Cbc => Self::Aes128Cbc,
            ms_ca::CwAlgo::CsaIcam => Self::CsaIcam,
        }
    }
}

/// Which implementation registered an algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// The hardware descrambler (`hw_csa_op`, `hw_aes_ecb_op`, `hw_aes_cbc_op`).
    Hardware,
    /// A software implementation (libdvbcsa CSA, OpenSSL AES).
    Software,
}

/// One registered op (`SCW_op`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AlgoOp {
    /// Algorithm handled.
    pub algo: Algo,
    /// Implementation handling it.
    pub backend: Backend,
}

/// `MAX_OPS` (pmt.cpp).
pub const MAX_OPS: usize = 10;

/// Ops in registration order; hardware registers first so it wins lookups.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AlgoRegistry {
    ops: Vec<AlgoOp>,
}

impl AlgoRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// `register_algo`: appends `op`; `false` once `MAX_OPS` are taken.
    pub fn register(&mut self, op: AlgoOp) -> bool {
        if self.ops.len() >= MAX_OPS {
            return false;
        }
        self.ops.push(op);
        true
    }

    /// `get_op_for_algo`: the first op registered for `algo`.
    pub fn op_for_algo(&self, algo: Algo) -> Option<&AlgoOp> {
        self.ops.iter().find(|op| op.algo == algo)
    }

    /// Registered ops in order.
    pub fn ops(&self) -> &[AlgoOp] {
        &self.ops
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_values_match_pmt_h() {
        for (algo, raw) in [
            (Algo::Csa, 0),
            (Algo::Des, 1),
            (Algo::Aes128Ecb, 2),
            (Algo::Aes128Cbc, 3),
            (Algo::CsaIcam, 4),
        ] {
            assert_eq!(algo.raw(), raw);
            assert_eq!(Algo::from_raw(raw), Some(algo));
        }
        assert_eq!(Algo::from_raw(5), None);
    }

    #[test]
    fn first_registration_wins_and_registry_is_bounded() {
        let mut reg = AlgoRegistry::new();
        let hw = AlgoOp {
            algo: Algo::Csa,
            backend: Backend::Hardware,
        };
        let sw = AlgoOp {
            algo: Algo::Csa,
            backend: Backend::Software,
        };
        assert!(reg.register(hw));
        assert!(reg.register(sw));
        assert_eq!(reg.op_for_algo(Algo::Csa), Some(&hw));
        assert_eq!(reg.op_for_algo(Algo::Des), None);
        for _ in 2..MAX_OPS {
            assert!(reg.register(sw));
        }
        assert!(!reg.register(sw));
        assert_eq!(reg.ops().len(), MAX_OPS);
    }
}
