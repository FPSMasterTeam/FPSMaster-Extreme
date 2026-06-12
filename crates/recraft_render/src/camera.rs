use glam::{Mat4, Vec3, Vec4};

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
        // Minecraft convention: positive pitch looks DOWN (look vector Y is
        // -sin(pitch)). This must match the pitch we send to the server, or the
        // server's interaction ray-traces (reach / rotation-place / break) aim
        // the opposite way vertically and flag every angled interaction.
        Vec3::new(
            -yaw.sin() * pitch.cos(),
            -pitch.sin(),
            yaw.cos() * pitch.cos(),
        )
        .normalize()
    }

    pub fn view_projection(&self) -> Mat4 {
        let view = Mat4::look_to_rh(self.position, self.direction(), Vec3::Y);
        let projection = Mat4::perspective_rh(
            self.fovy_degrees.to_radians(),
            self.aspect.max(0.001),
            self.z_near,
            self.z_far,
        );
        projection * view
    }

    pub fn frustum(&self) -> Frustum {
        Frustum::from_view_projection(self.view_projection())
    }
}

/// View frustum as six half-spaces, extracted from a view-projection matrix
/// (Gribb–Hartmann). Used to skip drawing chunks that are entirely off-screen.
#[derive(Debug, Clone, Copy)]
pub struct Frustum {
    planes: [Vec4; 6],
}

impl Frustum {
    pub fn from_view_projection(m: Mat4) -> Self {
        let r0 = m.row(0);
        let r1 = m.row(1);
        let r2 = m.row(2);
        let r3 = m.row(3);
        // wgpu uses a 0..1 clip-space depth range, so near = r2, far = r3 - r2.
        let planes = [
            r3 + r0, // left
            r3 - r0, // right
            r3 + r1, // bottom
            r3 - r1, // top
            r2,      // near
            r3 - r2, // far
        ];
        Self { planes }
    }

    /// Conservative test: returns false only when the axis-aligned box lies fully
    /// outside one of the frustum planes (never culls a box that is visible).
    pub fn intersects_aabb(&self, min: Vec3, max: Vec3) -> bool {
        for plane in &self.planes {
            // Pick the box corner furthest along the plane normal.
            let px = if plane.x >= 0.0 { max.x } else { min.x };
            let py = if plane.y >= 0.0 { max.y } else { min.y };
            let pz = if plane.z >= 0.0 { max.z } else { min.z };
            if plane.x * px + plane.y * py + plane.z * pz + plane.w < 0.0 {
                return false;
            }
        }
        true
    }
}
