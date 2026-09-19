//! Fly and map cameras. The world is the treemap on the XY plane; cells extrude along +Z. Both
//! cameras produce a wgpu-convention view-projection matrix (clip z in 0..1).

use glam::{Mat4, Vec3};

use flyover_tiles::Bounds;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraMode {
    /// Orbit/pan/zoom around a point on the map.
    Map,
    /// Free flight: position plus yaw/pitch, speed scales with altitude.
    Fly,
}

#[derive(Debug, Clone, Copy)]
pub struct Camera {
    pub mode: CameraMode,
    // Map camera.
    pub center: Vec3,
    pub distance: f32,
    pub yaw: f32,
    pub pitch: f32,
    // Fly camera.
    pub pos: Vec3,
    pub fly_yaw: f32,
    pub fly_pitch: f32,
    // Lens.
    pub fovy: f32,
    pub near: f32,
    pub far: f32,
}

impl Camera {
    /// A map camera framing the whole tile set from above and to one side.
    pub fn framing(bounds: Bounds) -> Self {
        let w = (bounds.max_x - bounds.min_x) as f32;
        let h = (bounds.max_y - bounds.min_y) as f32;
        let center = Vec3::new(
            (bounds.min_x as f32 + bounds.max_x as f32) * 0.5,
            (bounds.min_y as f32 + bounds.max_y as f32) * 0.5,
            0.0,
        );
        let span = w.max(h).max(1.0);
        Camera {
            mode: CameraMode::Map,
            center,
            distance: span * 1.4,
            yaw: std::f32::consts::FRAC_PI_4,
            pitch: -0.9,
            pos: center + Vec3::new(0.0, -span, span),
            fly_yaw: std::f32::consts::FRAC_PI_2,
            fly_pitch: -0.7,
            fovy: 60f32.to_radians(),
            near: (span * 0.001).max(0.05),
            far: span * 8.0,
        }
    }

    /// Direction the fly camera faces.
    pub fn fly_forward(&self) -> Vec3 {
        let (sy, cy) = self.fly_yaw.sin_cos();
        let (sp, cp) = self.fly_pitch.sin_cos();
        Vec3::new(cy * cp, sy * cp, sp).normalize_or_zero()
    }

    /// The eye position for the active mode.
    pub fn eye(&self) -> Vec3 {
        match self.mode {
            CameraMode::Map => {
                let (sy, cy) = self.yaw.sin_cos();
                let (sp, cp) = self.pitch.sin_cos();
                let dir = Vec3::new(cy * cp, sy * cp, sp);
                self.center - dir * self.distance
            }
            CameraMode::Fly => self.pos,
        }
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let eye = self.eye();
        let (target, up) = match self.mode {
            CameraMode::Map => (self.center, Vec3::Z),
            CameraMode::Fly => (eye + self.fly_forward(), Vec3::Z),
        };
        // Right-handed, Z up; DirectX-style 0..1 clip depth matches wgpu.
        let view = glam::camera::rh::view::look_at_mat4(eye, target, up);
        let proj = glam::camera::rh::proj::directx::perspective(
            self.fovy,
            aspect.max(0.01),
            self.near,
            self.far,
        );
        proj * view
    }
}
