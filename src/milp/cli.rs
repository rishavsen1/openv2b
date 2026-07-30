//! `LpCliBackend`: the universal, dependency-free backend. Writes the model
//! as an LP file, invokes any solver CLI on it, and parses the solution file.
//! Slower per solve than an in-process backend (process spawn, license check,
//! file I/O: typically 100-500 ms) but works with every serious solver:
//! CPLEX, Gurobi, Xpress, HiGHS, CBC, SCIP, Hexaly.

use super::{MilpBackend, Model, SolStatus, Solution, SolveError};
use std::path::PathBuf;
use std::process::Command;

/// How to read the solver's solution file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolutionFormat {
    /// CPLEX `write <file> sol` XML: `<variable name="x" ... value="1.5"/>`.
    CplexXml,
    /// Any format reducible to plain `<name> <value>` token pairs per line
    /// (HiGHS `--solution_file`, Gurobi `.sol`, CBC `solution` output).
    VarValueLines,
}

/// Invocation recipe. `{lp}` and `{sol}` in `args` are replaced by the model
/// and solution file paths.
#[derive(Debug, Clone)]
pub struct LpCliBackend {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    pub format: SolutionFormat,
    pub workdir: PathBuf,
}

impl LpCliBackend {
    /// CPLEX interactive-optimizer recipe, deterministic (threads 1, fixed
    /// seed). `cplex_bin` example: /opt/ibm/cplex/bin/x86-64_linux/cplex.
    pub fn cplex(cplex_bin: impl Into<String>, workdir: impl Into<PathBuf>) -> Self {
        LpCliBackend {
            name: "cplex-cli".into(),
            program: cplex_bin.into(),
            args: vec![
                "-c".into(),
                "read {lp}".into(),
                "set threads 1".into(),
                "set randomseed 100".into(),
                "optimize".into(),
                "write {sol}".into(),
            ],
            format: SolutionFormat::CplexXml,
            workdir: workdir.into(),
        }
    }

    /// HiGHS CLI recipe.
    pub fn highs(highs_bin: impl Into<String>, workdir: impl Into<PathBuf>) -> Self {
        LpCliBackend {
            name: "highs-cli".into(),
            program: highs_bin.into(),
            args: vec![
                "--solution_file".into(),
                "{sol}".into(),
                "--random_seed".into(),
                "100".into(),
                "{lp}".into(),
            ],
            format: SolutionFormat::VarValueLines,
            workdir: workdir.into(),
        }
    }
}

impl MilpBackend for LpCliBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn solve(&self, model: &Model) -> Result<Solution, SolveError> {
        std::fs::create_dir_all(&self.workdir)?;
        let lp_path = self.workdir.join("model.lp");
        let sol_path = self.workdir.join("model.sol");
        std::fs::write(&lp_path, model.to_lp())?;
        let _ = std::fs::remove_file(&sol_path);

        let args: Vec<String> = self
            .args
            .iter()
            .map(|a| {
                a.replace("{lp}", &lp_path.to_string_lossy())
                    .replace("{sol}", &sol_path.to_string_lossy())
            })
            .collect();
        let out = Command::new(&self.program)
            .args(&args)
            .current_dir(&self.workdir)
            .output()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.to_lowercase().contains("infeasible") {
            return Ok(Solution {
                status: SolStatus::Infeasible,
                objective: f64::NAN,
                values: vec![0.0; model.vars.len()],
            });
        }
        if !sol_path.exists() {
            return Err(SolveError::Backend(format!(
                "{}: no solution file produced; stdout tail: {}",
                self.name,
                &stdout[stdout.len().saturating_sub(500)..]
            )));
        }
        let text = std::fs::read_to_string(&sol_path)?;
        let values = match self.format {
            SolutionFormat::CplexXml => parse_cplex_xml(&text, model),
            SolutionFormat::VarValueLines => parse_var_value_lines(&text, model),
        };
        let objective = values
            .iter()
            .zip(&model.vars)
            .map(|(x, v)| x * v.obj)
            .sum::<f64>()
            + model.obj_constant;
        Ok(Solution {
            status: SolStatus::Optimal,
            objective,
            values,
        })
    }
}

/// Minimal scan of CPLEX solution XML: no XML dependency, just attribute
/// pairs on `<variable .../>` elements.
fn parse_cplex_xml(text: &str, model: &Model) -> Vec<f64> {
    let index: std::collections::HashMap<&str, usize> = model
        .vars
        .iter()
        .enumerate()
        .map(|(i, v)| (v.name.as_str(), i))
        .collect();
    let mut values = vec![0.0; model.vars.len()];
    for chunk in text.split("<variable ").skip(1) {
        let attr = |key: &str| -> Option<&str> {
            let pat = format!("{key}=\"");
            let start = chunk.find(&pat)? + pat.len();
            let end = chunk[start..].find('"')? + start;
            Some(&chunk[start..end])
        };
        if let (Some(name), Some(value)) = (attr("name"), attr("value")) {
            if let (Some(&i), Ok(x)) = (index.get(name), value.parse::<f64>()) {
                values[i] = x;
            }
        }
    }
    values
}

/// Any line whose first token names a model variable and whose second parses
/// as a number is taken as an assignment; everything else is ignored.
fn parse_var_value_lines(text: &str, model: &Model) -> Vec<f64> {
    let index: std::collections::HashMap<&str, usize> = model
        .vars
        .iter()
        .enumerate()
        .map(|(i, v)| (v.name.as_str(), i))
        .collect();
    let mut values = vec![0.0; model.vars.len()];
    for line in text.lines() {
        let mut tok = line.split_whitespace();
        if let (Some(name), Some(value)) = (tok.next(), tok.next()) {
            if let (Some(&i), Ok(x)) = (index.get(name), value.parse::<f64>()) {
                values[i] = x;
            }
        }
    }
    values
}
