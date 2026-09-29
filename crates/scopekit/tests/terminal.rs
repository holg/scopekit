//! Terminal mode end to end on a TestBackend with half blocks: a real GPU
//! view (a solid colour) rendered offscreen and drawn into the cells the
//! app placed it in. Skipped where no GPU adapter exists.

use scopekit::crossterm::event::Event;
use scopekit::ratatui::backend::TestBackend;
use scopekit::ratatui::layout::Rect;
use scopekit::ratatui::style::Color;
use scopekit::ratatui::widgets::Paragraph;
use scopekit::ratatui::{Frame, Terminal};
use scopekit::terminal::Driver;
use scopekit::{wgpu, App, Config, Flow, Gpu, GpuView, Protocol, Target, ViewSlot};
use std::cell::Cell;
use std::rc::Rc;

/// Clears its region to one colour; counts its renders.
struct Solid {
    rgb: [f64; 3],
    renders: Rc<Cell<u32>>,
    changed: bool,
}

impl GpuView for Solid {
    fn prepare(&mut self, _: &Gpu, _: wgpu::TextureFormat) {}

    fn render(&mut self, _: &Gpu, encoder: &mut wgpu::CommandEncoder, t: &Target<'_>) {
        let [r, g, b] = self.rgb;
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: t.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a: 1.0 }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_scissor_rect(t.region.x, t.region.y, t.region.width, t.region.height);
        self.renders.set(self.renders.get() + 1);
        self.changed = false;
    }

    fn changed(&self) -> bool {
        self.changed
    }
}

struct Screen {
    place: Option<Rect>,
}

impl App for Screen {
    fn draw(&mut self, f: &mut Frame, view: &mut ViewSlot) {
        f.render_widget(Paragraph::new("hello"), Rect::new(0, 0, 5, 1));
        if let Some(r) = self.place {
            view.place("solid", r);
        }
    }

    fn event(&mut self, _: Event) -> Flow {
        Flow::Continue
    }
}

#[test]
fn the_view_is_rendered_into_its_cells_and_only_when_needed() {
    if scopekit::gpu::headless(scopekit::Backend::Auto).is_err() {
        eprintln!("skipped: no GPU adapter");
        return;
    }
    let renders = Rc::new(Cell::new(0));
    // Pure green in sRGB is 0x00ff00 on screen.
    let (solid, view) = scopekit::share(Solid {
        rgb: [0.0, 1.0, 0.0],
        renders: renders.clone(),
        changed: true,
    });
    let config = Config {
        protocol: Protocol::Halfblocks,
        ..Config::default()
    };
    let picker = scopekit::terminal::picker(Protocol::Halfblocks);
    let mut driver = Driver::new(picker, scopekit::Views::new().with("solid", view), &config);
    let mut term = Terminal::new(TestBackend::new(40, 12)).unwrap();
    let area = Rect::new(10, 2, 20, 6);
    let mut app = Screen { place: Some(area) };

    driver.draw(&mut term, &mut app).unwrap();
    assert_eq!(renders.get(), 1, "{:?}", driver.error());
    let buf = term.backend().buffer();
    assert_eq!(buf[(0, 0)].symbol(), "h", "the app's text is there");
    let green = |x, y| {
        let c = &buf[(x, y)];
        [c.fg, c.bg].contains(&Color::Rgb(0, 255, 0))
    };
    assert!(green(15, 4), "the middle of the placed area shows the view");
    assert!(!green(2, 8), "outside the area it does not");

    // Nothing changed: no second render.
    driver.draw(&mut term, &mut app).unwrap();
    assert_eq!(renders.get(), 1);

    // New content: rendered again.
    solid.borrow_mut().changed = true;
    driver.draw(&mut term, &mut app).unwrap();
    assert_eq!(renders.get(), 2);

    // Not placed (a popup covers it): not rendered, not drawn.
    app.place = None;
    solid.borrow_mut().changed = true;
    driver.draw(&mut term, &mut app).unwrap();
    assert_eq!(renders.get(), 2);
}

#[test]
fn render_to_rgba_gives_exact_pixels() {
    let renders = Rc::new(Cell::new(0));
    let mut solid = Solid {
        rgb: [1.0, 0.0, 0.0],
        renders,
        changed: true,
    };
    match scopekit::render_to_rgba(&mut solid, 4, 3, scopekit::Backend::Auto) {
        Ok(px) => {
            assert_eq!(px.len(), 4 * 3 * 4);
            assert!(px.chunks(4).all(|p| p == [255, 0, 0, 255]));
        }
        Err(e) => eprintln!("skipped: {e}"),
    }
}

struct TwoViews;

impl App for TwoViews {
    fn draw(&mut self, _: &mut Frame, view: &mut ViewSlot) {
        view.place("left", Rect::new(0, 0, 10, 4));
        view.place("right", Rect::new(20, 0, 10, 4));
    }

    fn event(&mut self, _: Event) -> Flow {
        Flow::Continue
    }
}

#[test]
fn several_named_views_each_land_in_their_own_cells() {
    if scopekit::gpu::headless(scopekit::Backend::Auto).is_err() {
        eprintln!("skipped: no GPU adapter");
        return;
    }
    let renders = Rc::new(Cell::new(0));
    let (_, red) = scopekit::share(Solid {
        rgb: [1.0, 0.0, 0.0],
        renders: renders.clone(),
        changed: true,
    });
    let (_, blue) = scopekit::share(Solid {
        rgb: [0.0, 0.0, 1.0],
        renders: renders.clone(),
        changed: true,
    });
    let views = scopekit::Views::new().with("left", red).with("right", blue);
    let config = Config {
        protocol: Protocol::Halfblocks,
        ..Config::default()
    };
    let mut driver = Driver::new(
        scopekit::terminal::picker(Protocol::Halfblocks),
        views,
        &config,
    );
    let mut term = Terminal::new(TestBackend::new(40, 6)).unwrap();
    driver.draw(&mut term, &mut TwoViews).unwrap();
    assert_eq!(renders.get(), 2);
    let buf = term.backend().buffer();
    let has = |x, y, c| {
        let cell = &buf[(x, y)];
        [cell.fg, cell.bg].contains(&c)
    };
    assert!(has(5, 2, Color::Rgb(255, 0, 0)));
    assert!(has(25, 2, Color::Rgb(0, 0, 255)));
}

#[test]
fn placing_an_unregistered_view_is_an_error_not_a_panic() {
    let config = Config {
        protocol: Protocol::Halfblocks,
        ..Config::default()
    };
    let mut driver = Driver::new(
        scopekit::terminal::picker(Protocol::Halfblocks),
        scopekit::Views::new(),
        &config,
    );
    let mut term = Terminal::new(TestBackend::new(40, 6)).unwrap();
    let err = driver.draw(&mut term, &mut TwoViews).unwrap_err();
    assert!(err.contains("\"left\""), "{err}");
}
