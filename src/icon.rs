fn render_icon_rgba(size: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    let s = size as f32;
    let half = s * 0.5;

    for y in 0..size {
        for x in 0..size {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;

            // Normalized coordinates [-1, 1]
            let nx = (px - half) / half;
            let ny = (py - half) / half;

            // Rounded rectangle SDF for outer dark glass badge
            let rx = nx.abs() - 0.68;
            let ry = ny.abs() - 0.68;
            let outside_dist = rx.max(0.0).hypot(ry.max(0.0)) + rx.min(0.0).max(ry.min(0.0)) - 0.22;

            if outside_dist > 0.04 {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }

            // Anti-aliased outer edge alpha
            let bg_alpha = ((0.04 - outside_dist) / 0.08).clamp(0.0, 1.0);

            // Base dark shadcn zinc-900 surface (#141417) with subtle top-left glass sheen
            let sheen = ((-nx - ny) * 0.5 + 0.5).clamp(0.0, 1.0) * 18.0;
            let mut r = 18.0 + sheen;
            let mut g = 18.0 + sheen;
            let mut b = 21.0 + sheen;

            // Crisp specular rim
            if outside_dist > -0.09 {
                let rim = ((outside_dist + 0.09) / 0.09).clamp(0.0, 1.0);
                r += rim * 95.0;
                g += rim * 95.0;
                b += rim * 105.0;
            }

            // Inner mouse body capsule (vertical capsule centered at (0, 0.03))
            let mx = nx.abs() - 0.16;
            let my = (ny - 0.03).abs() - 0.28;
            let mouse_sdf = mx.max(0.0).hypot(my.max(0.0)) + mx.min(0.0).max(my.min(0.0)) - 0.22;

            if mouse_sdf <= 0.04 {
                let mouse_cov = ((0.04 - mouse_sdf) / 0.08).clamp(0.0, 1.0);
                // Mouse shell outline vs dark interior
                if mouse_sdf > -0.10 {
                    // Silver-white metallic rim (#e4e4e7)
                    r = r * (1.0 - mouse_cov) + 235.0 * mouse_cov;
                    g = g * (1.0 - mouse_cov) + 235.0 * mouse_cov;
                    b = b * (1.0 - mouse_cov) + 240.0 * mouse_cov;
                } else {
                    // Dark translucent interior (#27272a)
                    r = r * (1.0 - mouse_cov) + 39.0 * mouse_cov;
                    g = g * (1.0 - mouse_cov) + 39.0 * mouse_cov;
                    b = b * (1.0 - mouse_cov) + 44.0 * mouse_cov;
                }
            }

            // Scroll wheel / active telemetry indicator pill near top of mouse
            let wx = nx.abs();
            let wy = (ny + 0.15).abs() - 0.09;
            let wheel_sdf = wx.max(0.0).hypot(wy.max(0.0)) + wx.min(0.0).max(wy.min(0.0)) - 0.07;
            if wheel_sdf <= 0.04 {
                let wheel_cov = ((0.04 - wheel_sdf) / 0.08).clamp(0.0, 1.0);
                // Crisp emerald-silver telemetry dot (#34d399)
                r = r * (1.0 - wheel_cov) + 52.0 * wheel_cov;
                g = g * (1.0 - wheel_cov) + 211.0 * wheel_cov;
                b = b * (1.0 - wheel_cov) + 153.0 * wheel_cov;
            }

            rgba.push(r.clamp(0.0, 255.0) as u8);
            rgba.push(g.clamp(0.0, 255.0) as u8);
            rgba.push(b.clamp(0.0, 255.0) as u8);
            rgba.push((bg_alpha * 255.0).clamp(0.0, 255.0) as u8);
        }
    }

    rgba
}

pub fn create_tray_icon() -> Result<tray_icon::Icon, String> {
    let size = 32;
    let rgba = render_icon_rgba(size);
    tray_icon::Icon::from_rgba(rgba, size, size).map_err(|e| format!("Tray icon error: {e}"))
}

pub fn create_window_icon() -> Result<tao::window::Icon, String> {
    let size = 32;
    let rgba = render_icon_rgba(size);
    tao::window::Icon::from_rgba(rgba, size, size).map_err(|e| format!("Window icon error: {e}"))
}
