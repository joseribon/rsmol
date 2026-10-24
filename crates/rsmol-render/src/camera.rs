use glam::camera::rh::{proj::directx, view::look_at_mat4};
use glam::{Mat4, Quat, Vec3};

/// Orbit camera: rotates around `target` at `distance`, looking down its local -Z axis.
///
/// The scene's bounding sphere (`scene_center`, `scene_radius`) sets the clip planes, so
/// focusing on part of a structure never clips the rest away.
#[derive(Clone, Debug)]
pub struct Camera {
    pub target: Vec3,
    pub distance: f32,
    pub rotation: Quat,
    /// Vertical field of view in radians.
    pub fov_y: f32,
    pub scene_center: Vec3,
    pub scene_radius: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            target: Vec3::ZERO,
            distance: 50.0,
            rotation: Quat::IDENTITY,
            fov_y: 30f32.to_radians(),
            scene_center: Vec3::ZERO,
            scene_radius: 20.0,
        }
    }
}

/// Centroid and bounding radius of `points`, padded for atom radii.
fn bounding_sphere(points: &[[f32; 3]]) -> Option<(Vec3, f32)> {
    if points.is_empty() {
        return None;
    }
    let centroid = points.iter().fold(Vec3::ZERO, |acc, &p| acc + Vec3::from(p)) / points.len() as f32;
    let radius = points.iter().map(|&p| Vec3::from(p).distance_squared(centroid)).fold(0.0, f32::max).sqrt();
    Some((centroid, (radius + 2.0).max(1.0)))
}

impl Camera {
    /// Makes `points` the whole scene and frames all of it.
    pub fn fit(&mut self, points: &[[f32; 3]], aspect: f32) {
        if let Some((center, radius)) = bounding_sphere(points) {
            self.scene_center = center;
            self.scene_radius = radius;
            self.frame(center, radius, aspect);
        }
    }

    /// Frames `points` (a selection) without changing the scene bounds or the rotation.
    pub fn focus(&mut self, points: &[[f32; 3]], aspect: f32) {
        if let Some((center, radius)) = bounding_sphere(points) {
            self.frame(center, radius, aspect);
        }
    }

    /// Backs off until a sphere fills the shorter viewport dimension.
    fn frame(&mut self, center: Vec3, radius: f32, aspect: f32) {
        self.target = center;
        let half_fov = (self.fov_y * 0.5).min((aspect * (self.fov_y * 0.5).tan()).atan());
        self.distance = radius / half_fov.sin();
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

    fn eye(&self) -> Vec3 {
        self.target + self.rotation * Vec3::Z * self.distance
    }

    /// View-space distances over which depth fog fades in: from just in front of the scene's
    /// center to its far side.
    pub fn depth_range(&self) -> (f32, f32) {
        let to_center = self.eye().distance(self.scene_center);
        (to_center - self.scene_radius * 0.2, to_center + self.scene_radius)
    }

    /// Pixel position of world point `p` in a `width`×`height` viewport (origin top-left), or
    /// `None` when it is behind the camera or outside the clip volume.
    pub fn project(&self, p: [f32; 3], width: f32, height: f32) -> Option<[f32; 2]> {
        let clip = self.proj(width / height.max(1.0)) * self.view() * Vec3::from(p).extend(1.0);
        if clip.w <= 0.0 {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        if ndc.x.abs() > 1.0 || ndc.y.abs() > 1.0 || !(0.0..=1.0).contains(&ndc.z) {
            return None;
        }
        Some([(ndc.x + 1.0) * 0.5 * width, (1.0 - ndc.y) * 0.5 * height])
    }

    pub fn view(&self) -> Mat4 {
        look_at_mat4(self.eye(), self.target, self.rotation * Vec3::Y)
    }

    pub fn proj(&self, aspect: f32) -> Mat4 {
        // Clip planes hug the scene's bounding sphere as seen from the eye, for depth precision.
        let to_center = self.eye().distance(self.scene_center);
        let reach = self.scene_radius + 1.0;
        let far = to_center + reach;
        // Inside the scene (zoomed into a pocket), keep a small near plane relative to the
        // focus distance rather than clipping at the eye.
        let near = (to_center - reach).max(self.distance * 0.05).max(0.05);
        // WebGPU clip space matches the D3D convention: y up, depth in [0, 1].
        directx::perspective(self.fov_y, aspect, near, far)
    }
}
