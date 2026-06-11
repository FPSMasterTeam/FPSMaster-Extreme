use glam::{Mat4, Vec3};

#[derive(Debug, Clone, Copy)]
pub struct Camera {
    pub position: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub aspect: f32,
    pub fovy_degrees: f32,
    pub z_near: f32,
    pub z_far: f32,
}

impl Camera {
    pub fn new(position: Vec3, aspect: f32) -> Self {
        Self {
            position,
            yaw: -90.0,
            pitch: -25.0,
            aspect,
            fovy_degrees: 70.0,
            z_near: 0.05,
            z_far: 1000.0,
        }
    }

    pub fn direction(&self) -> Vec3 {
        let yaw = self.yaw.to_radians();
        let pitch = self.pitch.to_radians();
        Vec3::new(yaw.cos() * pitch.cos(), pitch.sin(), yaw.sin() * pitch.cos()).normalize()
    }

    pub fn view_projection(&self) -> Mat4 {
        let view = Mat4::look_to_rh(self.position, self.direction(), Vec3::Y);
        let projection = Mat4::perspective_rh(self.fovy_degrees.to_radians(), self.aspect.max(0.001), self.z_near, self.z_far);
        projection * view
    }
}
