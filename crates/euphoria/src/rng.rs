//! The per-character random generator (spec: braceForImpact.md, "Random generator").
//! One per character, shared by every behaviour on him.

/// 32-bit multiply-with-carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rng {
    pub x: u32,
    pub c: u32,
    /// Draws taken so far (not part of the generator; for tests and debugging).
    pub draws: u64,
}

const MUL: u64 = 0x5CDC_FAA7;

impl Rng {
    pub fn new(x: u32, c: u32) -> Self {
        Rng { x, c, draws: 0 }
    }

    /// Steps the generator and returns the new x.
    fn step(&mut self) -> u32 {
        let t = self.x as u64 * MUL + self.c as u64;
        self.x = t as u32;
        self.c = (t >> 32) as u32;
        self.draws += 1;
        self.x
    }

    /// Uniform in [0, 1), from the low 23 bits of the new x.
    pub fn uniform(&mut self) -> f32 {
        (self.step() & 0x7F_FFFF) as f32 * (1.0 / 8_388_608.0)
    }

    /// Bit 0 of the new x. Uses up one draw.
    pub fn coin(&mut self) -> bool {
        self.step() & 1 == 1
    }
}
