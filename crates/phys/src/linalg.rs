//! Small dense linear algebra for the joint-space system (n ≲ 80).

/// Row-major n×n matrix.
#[derive(Clone, Debug)]
pub struct Mat {
    pub n: usize,
    pub a: Vec<f64>,
}

impl Mat {
    pub fn zeros(n: usize) -> Mat {
        Mat { n, a: vec![0.0; n * n] }
    }
    #[inline]
    pub fn at(&self, i: usize, j: usize) -> f64 {
        self.a[i * self.n + j]
    }
    #[inline]
    pub fn add(&mut self, i: usize, j: usize, v: f64) {
        self.a[i * self.n + j] += v;
    }
}

/// Cholesky factor L (lower) of a symmetric positive-definite matrix, in place of a copy.
#[derive(Clone, Debug)]
pub struct Chol {
    n: usize,
    l: Vec<f64>,
}

impl Chol {
    pub fn new(m: &Mat) -> Chol {
        let n = m.n;
        let mut l = vec![0.0; n * n];
        for i in 0..n {
            for j in 0..=i {
                let mut s = m.at(i, j);
                for k in 0..j {
                    s -= l[i * n + k] * l[j * n + k];
                }
                if i == j {
                    l[i * n + i] = s.max(1e-12).sqrt();
                } else {
                    l[i * n + j] = s / l[j * n + j];
                }
            }
        }
        Chol { n, l }
    }

    /// Solves A x = b.
    pub fn solve(&self, b: &[f64]) -> Vec<f64> {
        let n = self.n;
        let mut y = b.to_vec();
        for i in 0..n {
            let mut s = y[i];
            for k in 0..i {
                s -= self.l[i * n + k] * y[k];
            }
            y[i] = s / self.l[i * n + i];
        }
        for i in (0..n).rev() {
            let mut s = y[i];
            for k in i + 1..n {
                s -= self.l[k * n + i] * y[k];
            }
            y[i] = s / self.l[i * n + i];
        }
        y
    }
}

pub fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
