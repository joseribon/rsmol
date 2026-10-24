use glam::camera::rh::{proj::directx, view::look_at_mat4};
use glam::{Mat4, Quat, Vec3};

/// Orbit camera: rotates around `target` at `distance`, looking down its local -Z axis.
#[derive(Clone, Debug)]
pub struct Camera {
    pub target: Vec3,
    pub distance: f32,
    pub rotation: Quat,
    /// Vertical field of view in radians.
    pub fov_y: f32,
    /// Radius of the scene around `target`, used to keep near/far planes tight.
    pub scene_radius: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            target: Vec3::ZERO,
            distance: 50.0,
            rotation: Quat::IDENTITY,
            fov_y: 30f32.to_radians(),
            scene_radius: 20.0,
        }
    }
}

impl Camera {
    /// Centers on the points' centroid and backs off until their bounding sphere fills the
    /// shorter viewport dimension.
    pub fn fit(&mut self, points: &[[f32; 3]], aspect: f32) {
        if points.is_empty() {
            return;
        }
        let centroid = points.iter().fold(Vec3::ZERO, |acc, &p| acc + Vec3::from(p)) / points.len() as f32;
        let radius = points.iter().map(|&p| Vec3::from(p).distance_squared(centroid)).fold(0.0, f32::max).sqrt();
        self.target = centroid;
        // Include the atom radius so edge atoms are not cut off.
        self.scene_radius = (radius + 2.0).max(1.0);
        let half_fov = (self.fov_y * 0.5).min((aspect * (self.fov_y * 0.5).tan()).atan());
        self.distance = self.scene_radius / half_fov.sin();
    }

    /// Trackball-style rotation from a mouse drag, in pixels.
    pub fn orbit(&mut self, dx: f32, dy: f32) {
        const RADIANS_PER_PIXEL: f32 = 0.005;
        let yaw = Quat::from_axis_angle(Vec3::Y, -dx * RADIANS_PER_PIXEL);
        let pitch = Quat::from_axis_angle(Vec3::X, -dy * RADIANS_PER_PIXEL);
        // Rotations are about the camera's own axes, so the drag direction always matches.
        self.rotation = (self.rotation * yaw * pitch).normalize();
    }

    /// Moves the target in the view plane; `dx`/`dy` in pixels for a viewport `height` tall.
    pub fn pan(&mut self, dx: f32, dy: f32, height: f32) {
        let world_per_pixel = 2.0 * self.distance * (self.fov_y * 0.5).tan() / height.max(1.0);
        self.target += self.rotation * Vec3::new(-dx, dy, 0.0) * world_per_pixel;
    }

    /// Multiplies the distance; `factor < 1` zooms in.
    pub fn zoom(&mut self, factor: f32) {
        self.distance = (self.distance * factor).clamp(1.0, self.scene_radius * 20.0);
    }

    pub fn view(&self) -> Mat4 {
        let eye = self.target + self.rotation * Vec3::Z * self.distance;
        look_at_mat4(eye, self.target, self.rotation * Vec3::Y)
    }

    pub fn proj(&self, aspect: f32) -> Mat4 {
        // Clip planes hug the scene for depth precision; atoms poke out of the box by ~2 Å.
        let reach = self.scene_radius + 3.0;
        let near = (self.distance - reach).max(self.distance * 0.01).max(0.05);
        let far = self.distance + reach;
        // WebGPU clip space matches the D3D convention: y up, depth in [0, 1].
        directx::perspective(self.fov_y, aspect, near, far)
    }
}
