//! In-process HiGHS backend (cargo feature `solver-highs`). MIT-licensed
//! solver, compiled into the binary: no process spawn, no license check,
//! microsecond handoff. The recommended default for MPC sweeps.

use super::{MilpBackend, Model, Sense, SolStatus, Solution, SolveError};

pub struct HighsBackend;

impl MilpBackend for HighsBackend {
    fn name(&self) -> &str {
        "highs"
    }

    fn solve(&self, model: &Model) -> Result<Solution, SolveError> {
        let mut pb = highs::RowProblem::default();
        let cols: Vec<highs::Col> = model
            .vars
            .iter()
            .map(|v| {
                if v.integer {
                    pb.add_integer_column(v.obj, v.lb..=v.ub)
                } else {
                    pb.add_column(v.obj, v.lb..=v.ub)
                }
            })
            .collect();
        for c in &model.constraints {
            let terms: Vec<(highs::Col, f64)> = c
                .terms
                .iter()
                .map(|&(vid, coef)| (cols[vid.0], coef))
                .collect();
            match c.sense {
                Sense::Le => pb.add_row(..=c.rhs, terms),
                Sense::Ge => pb.add_row(c.rhs.., terms),
                Sense::Eq => pb.add_row(c.rhs..=c.rhs, terms),
            }
        }
        let mut hm = pb.optimise(highs::Sense::Minimise);
        hm.set_option("threads", 1);
        hm.set_option("random_seed", 100);
        hm.make_quiet();
        let solved = hm.solve();
        match solved.status() {
            highs::HighsModelStatus::Optimal => {
                let sol = solved.get_solution();
                let values: Vec<f64> = sol.columns().to_vec();
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
            highs::HighsModelStatus::Infeasible => Ok(Solution {
                status: SolStatus::Infeasible,
                objective: f64::NAN,
                values: vec![0.0; model.vars.len()],
            }),
            other => Err(SolveError::Backend(format!("HiGHS status {other:?}"))),
        }
    }
}
