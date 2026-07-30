//! Backend-neutral linear/mixed-integer model + the `MilpBackend` trait.
//!
//! The formulation code (e.g. the MPC policy) builds a [`Model`] once per
//! solve; a backend turns it into a [`Solution`]. Backends: [`LpCliBackend`]
//! (no dependencies, drives any solver CLI via LP files) is always available;
//! in-process backends (HiGHS, Gurobi) live behind cargo features.

pub mod cli;
#[cfg(feature = "solver-highs")]
pub mod highs_backend;

/// Index of a variable in its model (dense, insertion-ordered).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VarId(pub usize);

#[derive(Debug, Clone)]
pub struct VarDef {
    pub name: String,
    pub lb: f64,
    pub ub: f64,
    pub integer: bool,
    /// Objective coefficient (minimization).
    pub obj: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sense {
    Le,
    Ge,
    Eq,
}

#[derive(Debug, Clone)]
pub struct Constraint {
    pub name: String,
    pub terms: Vec<(VarId, f64)>,
    pub sense: Sense,
    pub rhs: f64,
}

/// A minimization model. Insertion order is the serialization order, so LP
/// files (and therefore CLI-solver behavior) are deterministic.
#[derive(Debug, Clone, Default)]
pub struct Model {
    pub vars: Vec<VarDef>,
    pub constraints: Vec<Constraint>,
    /// Constant added to the objective (bookkeeping only).
    pub obj_constant: f64,
}

impl Model {
    pub fn add_var(&mut self, name: impl Into<String>, lb: f64, ub: f64, obj: f64) -> VarId {
        self.vars.push(VarDef {
            name: name.into(),
            lb,
            ub,
            integer: false,
            obj,
        });
        VarId(self.vars.len() - 1)
    }

    pub fn add_constraint(
        &mut self,
        name: impl Into<String>,
        terms: Vec<(VarId, f64)>,
        sense: Sense,
        rhs: f64,
    ) {
        self.constraints.push(Constraint {
            name: name.into(),
            terms,
            sense,
            rhs,
        });
    }

    /// Serialize as CPLEX-LP format (readable by CPLEX, Gurobi, HiGHS, CBC,
    /// SCIP, Xpress). All bounds are written explicitly: the LP-format
    /// default of [0, +inf) is a classic silent-bug source.
    pub fn to_lp(&self) -> String {
        use std::fmt::Write;
        let mut out = String::with_capacity(4096);
        out.push_str("Minimize\n obj:");
        let mut first = true;
        for v in &self.vars {
            if v.obj != 0.0 {
                let _ = write!(out, " {} {}", sign_num(v.obj, first), v.name);
                first = false;
            }
        }
        if first {
            // LP format requires at least one objective term.
            let _ = write!(
                out,
                " 0 {}",
                self.vars.first().map(|v| v.name.as_str()).unwrap_or("x0")
            );
        }
        out.push_str("\nSubject To\n");
        for c in &self.constraints {
            let _ = write!(out, " {}:", c.name);
            let mut first = true;
            for &(VarId(i), coef) in &c.terms {
                if coef != 0.0 {
                    let _ = write!(out, " {} {}", sign_num(coef, first), self.vars[i].name);
                    first = false;
                }
            }
            if first {
                let _ = write!(out, " 0 {}", self.vars[0].name);
            }
            let op = match c.sense {
                Sense::Le => "<=",
                Sense::Ge => ">=",
                Sense::Eq => "=",
            };
            let _ = writeln!(out, " {op} {}", lp_num(c.rhs));
        }
        out.push_str("Bounds\n");
        for v in &self.vars {
            if v.lb == f64::NEG_INFINITY && v.ub == f64::INFINITY {
                let _ = writeln!(out, " {} free", v.name);
            } else {
                let lb = if v.lb == f64::NEG_INFINITY {
                    "-inf".into()
                } else {
                    lp_num(v.lb)
                };
                let ub = if v.ub == f64::INFINITY {
                    "+inf".into()
                } else {
                    lp_num(v.ub)
                };
                let _ = writeln!(out, " {lb} <= {} <= {ub}", v.name);
            }
        }
        let generals: Vec<&str> = self
            .vars
            .iter()
            .filter(|v| v.integer)
            .map(|v| v.name.as_str())
            .collect();
        if !generals.is_empty() {
            out.push_str("Generals\n");
            for g in generals {
                let _ = writeln!(out, " {g}");
            }
        }
        out.push_str("End\n");
        out
    }
}

fn lp_num(x: f64) -> String {
    // Full round-trip precision; LP readers accept scientific notation.
    format!("{x:.17e}")
}

fn sign_num(x: f64, first: bool) -> String {
    if x < 0.0 {
        format!("- {}", lp_num(-x))
    } else if first {
        lp_num(x)
    } else {
        format!("+ {}", lp_num(x))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolStatus {
    Optimal,
    Infeasible,
    Other,
}

#[derive(Debug, Clone)]
pub struct Solution {
    pub status: SolStatus,
    pub objective: f64,
    /// One value per variable, model insertion order. Variables absent from
    /// a solver's solution file default to 0 (LP solution files commonly omit
    /// zero-valued columns).
    pub values: Vec<f64>,
}

#[derive(Debug)]
pub enum SolveError {
    Io(std::io::Error),
    Backend(String),
}

impl std::fmt::Display for SolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SolveError::Io(e) => write!(f, "solver io error: {e}"),
            SolveError::Backend(m) => write!(f, "solver error: {m}"),
        }
    }
}

impl std::error::Error for SolveError {}

impl From<std::io::Error> for SolveError {
    fn from(e: std::io::Error) -> Self {
        SolveError::Io(e)
    }
}

/// A solver capable of handling [`Model`]s. Implementations must be
/// deterministic (pin threads and seeds where the underlying API allows).
pub trait MilpBackend {
    fn name(&self) -> &str;
    fn solve(&self, model: &Model) -> Result<Solution, SolveError>;
}
