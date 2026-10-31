#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
use eframe::egui;
use mygrep::{Encoding, Event, Hit, Options, Stats};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
        Arc,
    },
    time::{Duration, Instant},
};
struct App {
    options: Options,
    hits: Vec<Hit>,
    warnings: Vec<String>,
    warning_count: usize,
    stats: Stats,
    rx: Option<Receiver<Event>>,
    cancel: Arc<AtomicBool>,
    status: String,
    selected: Option<usize>,
    started: Option<Instant>,
    elapsed: f64,
    help: bool,
}
impl Default for App {
    fn default() -> Self {
        Self {
            options: Options::default(),
            hits: vec![],
            warnings: vec![],
            warning_count: 0,
            stats: Stats::default(),
            rx: None,
            cancel: Arc::new(AtomicBool::new(false)),
            status: "Оберіть файли або папку й введіть пошуковий запит.".into(),
            selected: None,
            started: None,
            elapsed: 0.,
            help: false,
        }
    }
}
impl App {
    fn add_files(&mut self) {
        if let Some(files) = rfd::FileDialog::new()
            .set_title("Оберіть текстові файли")
            .pick_files()
        {
            for p in files {
                if !self.options.roots.contains(&p) {
                    self.options.roots.push(p)
                }
            }
        }
    }
    fn add_folder(&mut self) {
        if let Some(p) = rfd::FileDialog::new()
            .set_title("Папка для пошуку")
            .pick_folder()
        {
            if !self.options.roots.contains(&p) {
                self.options.roots.push(p)
            }
        }
    }
    fn start(&mut self) {
        if self.options.roots.is_empty() {
            self.status = "Спочатку оберіть файли або папку.".into();
            return;
        }
        if self.options.extensions.trim().is_empty() {
            self.status = "Задайте розширення або * для всіх текстових файлів.".into();
            return;
        }
        let re = match mygrep::matcher(&self.options) {
            Ok(r) => r,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        self.hits.clear();
        self.warnings.clear();
        self.warning_count = 0;
        self.selected = None;
        self.stats = Stats::default();
        self.cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel(32);
        self.rx = Some(rx);
        let opts = self.options.clone();
        let cancel = self.cancel.clone();
        std::thread::spawn(move || mygrep::search(opts, re, cancel, tx));
        self.started = Some(Instant::now());
        self.status = "Виконується пошук…".into();
    }
    fn save(&mut self, csv: bool) {
        if let Some(p) = rfd::FileDialog::new()
            .set_file_name(if csv {
                "mygrep_results.csv"
            } else {
                "mygrep_results.txt"
            })
            .add_filter(
                if csv { "CSV" } else { "Текст" },
                &[if csv { "csv" } else { "txt" }],
            )
            .save_file()
        {
            self.status = match mygrep::export(&p, &self.hits, csv) {
                Ok(()) => format!("Збережено: {}", p.display()),
                Err(e) => format!("Помилка збереження: {e}"),
            };
        }
    }
    fn poll(&mut self) {
        let mut done = false;
        if let Some(rx) = &self.rx {
            for _ in 0..256 {
                match rx.try_recv() {
                    Ok(Event::Hits(mut rows)) => self.hits.append(&mut rows),
                    Ok(Event::Warning(w)) => {
                        self.warning_count += 1;
                        if self.warnings.len() < 500 {
                            self.warnings.push(w)
                        }
                    }
                    Ok(Event::Progress(s)) => self.stats = s,
                    Ok(Event::Done(s)) => {
                        self.stats = s;
                        done = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.status = "Пошуковий потік завершився без підсумку.".into();
                        self.started = None;
                        done = true;
                        break;
                    }
                }
            }
        }
        if done {
            self.rx = None;
            if let Some(start) = self.started.take() {
                self.elapsed = start.elapsed().as_secs_f64();
                self.status = if self.stats.stopped {
                    "Пошук зупинено. Показано часткові результати."
                } else if self.stats.limited {
                    "Досягнуто ліміт результатів. Звузьте запит або збільште ліміт."
                } else {
                    "Пошук завершено."
                }
                .into();
            }
        }
    }
}
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        let busy = self.rx.is_some();
        if busy {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("Файл", |ui| {
                    ui.add_enabled_ui(!busy, |ui| {
                        if ui.button("Додати файли…").clicked() {
                            self.add_files();
                            ui.close_menu()
                        }
                        if ui.button("Додати папку…").clicked() {
                            self.add_folder();
                            ui.close_menu()
                        }
                    });
                    ui.separator();
                    ui.add_enabled_ui(!busy && !self.hits.is_empty(), |ui| {
                        if ui.button("Експорт TXT…").clicked() {
                            self.save(false);
                            ui.close_menu()
                        }
                        if ui.button("Експорт CSV…").clicked() {
                            self.save(true);
                            ui.close_menu()
                        }
                    });
                    ui.separator();
                    if ui.button("Вихід").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close)
                    }
                });
                if ui.button("Довідка").clicked() {
                    self.help = true
                }
                ui.separator();
                ui.label("mygrep  ·  Пошук у текстових файлах");
            });
        });
        egui::TopBottomPanel::bottom("status").show(ctx,|ui|{ui.label(&self.status);ui.horizontal_wrapped(|ui|{if busy{ui.spinner();}ui.label(format!("Опрацьовано: {}  |  Пропущено з попередженням: {}  |  Знайдених рядків: {}  |  Час: {:.1} с",self.stats.scanned,self.stats.skipped,self.hits.len(),self.started.map(|s|s.elapsed().as_secs_f64()).unwrap_or(self.elapsed)));});});
        egui::SidePanel::left("settings")
            .resizable(true)
            .default_width(350.)
            .width_range(300.0..=600.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("settings_scroll")
                    .show(ui, |ui| {
                        ui.heading("Параметри пошуку");
                        ui.add_space(8.);
                        ui.add_enabled_ui(!busy, |ui| {
                            ui.horizontal(|ui| {
                                if ui.button("＋ Файли").clicked() {
                                    self.add_files()
                                }
                                if ui.button("＋ Папка").clicked() {
                                    self.add_folder()
                                }
                                if ui.button("Прибрати всі").clicked() {
                                    self.options.roots.clear()
                                }
                            });
                            ui.label(format!("Вибрано джерел: {}", self.options.roots.len()));
                            let mut remove = None;
                            egui::ScrollArea::vertical()
                                .id_salt("roots")
                                .max_height(130.)
                                .show(ui, |ui| {
                                    for (i, p) in self.options.roots.iter().enumerate() {
                                        ui.horizontal(|ui| {
                                            if ui.small_button("×").clicked() {
                                                remove = Some(i)
                                            }
                                            ui.add(
                                                egui::Label::new(p.display().to_string())
                                                    .truncate(),
                                            )
                                            .on_hover_text(p.display().to_string());
                                        });
                                    }
                                });
                            if let Some(i) = remove {
                                self.options.roots.remove(i);
                            }
                            ui.separator();
                            ui.label("Що шукаємо:");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.options.query)
                                    .hint_text("Слово, фраза або регулярний вираз")
                                    .desired_width(f32::INFINITY),
                            );
                            ui.checkbox(&mut self.options.regex, "Регулярний вираз");
                            ui.checkbox(&mut self.options.case_sensitive, "Ураховувати регістр");
                            ui.checkbox(&mut self.options.whole_word, "Лише ціле слово");
                            ui.checkbox(
                                &mut self.options.invert,
                                "Рядки, які НЕ відповідають запиту",
                            );
                            ui.checkbox(&mut self.options.recursive, "Шукати у вкладених папках");
                            ui.separator();
                            ui.label("Розширення через пробіл, ; або ,:");
                            ui.add(
                                egui::TextEdit::multiline(&mut self.options.extensions)
                                    .desired_rows(3)
                                    .desired_width(f32::INFINITY),
                            );
                            if ui.small_button("Стандартні розширення").clicked()
                            {
                                self.options.extensions = mygrep::DEFAULT_EXTENSIONS.into()
                            }
                            egui::ComboBox::from_label("Кодування")
                                .selected_text(self.options.encoding.label())
                                .show_ui(ui, |ui| {
                                    for e in [
                                        Encoding::Auto,
                                        Encoding::Utf8,
                                        Encoding::Utf16Le,
                                        Encoding::Utf16Be,
                                        Encoding::Windows1251,
                                    ] {
                                        ui.selectable_value(
                                            &mut self.options.encoding,
                                            e,
                                            e.label(),
                                        );
                                    }
                                });
                            ui.horizontal(|ui| {
                                ui.label("Макс. розмір файла, МБ:");
                                ui.add(
                                    egui::DragValue::new(&mut self.options.max_mb).range(1..=2048),
                                );
                            });
                            ui.horizontal(|ui| {
                                ui.label("Ліміт знайдених рядків:");
                                ui.add(
                                    egui::DragValue::new(&mut self.options.max_results)
                                        .range(1..=1_000_000),
                                );
                            });
                        });
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    !busy,
                                    egui::Button::new("▶ Пошук").min_size(egui::vec2(110., 32.)),
                                )
                                .clicked()
                            {
                                self.start()
                            }
                            if ui
                                .add_enabled(busy, egui::Button::new("■ Зупинити"))
                                .clicked()
                            {
                                self.cancel.store(true, Ordering::Relaxed);
                                self.status = "Зупинення…".into();
                            }
                        });
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(!busy, egui::Button::new("Очистити результати"))
                                .clicked()
                            {
                                self.hits.clear();
                                self.selected = None;
                                self.warnings.clear();
                                self.warning_count = 0;
                                self.stats = Stats::default();
                                self.status = "Результати очищено.".into();
                            }
                        });
                        ui.add_enabled_ui(!busy && !self.hits.is_empty(), |ui| {
                            ui.horizontal(|ui| {
                                if ui.button("Експорт TXT").clicked() {
                                    self.save(false)
                                }
                                if ui.button("Експорт CSV").clicked() {
                                    self.save(true)
                                }
                            });
                        });
                    });
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Результати");
            ui.label("Оберіть рядок, щоб переглянути повний текст і шлях до файла.");
            ui.separator();
            ui.horizontal(|ui| {
                ui.add_sized([220., 20.], egui::Label::new("Файл"));
                ui.add_sized([65., 20.], egui::Label::new("Рядок"));
                ui.label("Текст");
            });
            let height = (ui.available_height() - 220.).max(90.);
            egui::ScrollArea::vertical()
                .id_salt("results")
                .max_height(height)
                .show_rows(ui, 24., self.hits.len(), |ui, range| {
                    for i in range {
                        let h = &self.hits[i];
                        ui.horizontal(|ui| {
                            let name = h.path.file_name().unwrap_or_default().to_string_lossy();
                            if ui
                                .add_sized(
                                    [220., 22.],
                                    egui::Button::new(name.as_ref())
                                        .selected(self.selected == Some(i)),
                                )
                                .on_hover_text(h.path.display().to_string())
                                .clicked()
                            {
                                self.selected = Some(i)
                            }
                            ui.add_sized([65., 22.], egui::Label::new(h.line.to_string()));
                            let preview: String = h.text.chars().take(250).collect();
                            if ui
                                .add(
                                    egui::Label::new(preview)
                                        .truncate()
                                        .sense(egui::Sense::click()),
                                )
                                .clicked()
                            {
                                self.selected = Some(i)
                            }
                        });
                    }
                });
            ui.separator();
            if let Some(h) = self.selected.and_then(|i| self.hits.get(i)) {
                ui.label(format!(
                    "{}  |  рядок {}  |  {}",
                    h.path.display(),
                    h.line,
                    h.encoding
                ));
                ui.horizontal(|ui| {
                    if ui.button("Копіювати рядок").clicked() {
                        ui.ctx().copy_text(h.text.clone())
                    }
                    if ui.button("Копіювати шлях").clicked() {
                        ui.ctx().copy_text(h.path.display().to_string())
                    }
                });
                egui::ScrollArea::both()
                    .id_salt("preview")
                    .max_height(110.)
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(&h.text).monospace())
                                .selectable(true),
                        );
                    });
            } else {
                ui.label("Рядок не вибрано.");
            }
            egui::CollapsingHeader::new(format!("Попередження ({})", self.warning_count)).show(
                ui,
                |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("warnings")
                        .max_height(100.)
                        .show(ui, |ui| {
                            for w in &self.warnings {
                                ui.label(w);
                            }
                            if self.warning_count > 500 {
                                ui.label("Показано перші 500 попереджень.");
                            }
                        });
                },
            );
        });
        if self.help {
            egui::Window::new("Довідка").open(&mut self.help).resizable(true).default_width(540.).show(ctx,|ui|{ui.label("1. Додайте файли або папку.\n2. Введіть слово чи фразу.\n3. Оберіть параметри й натисніть «Пошук».\n4. Оберіть результат для перегляду або експортуйте таблицю.");ui.separator();ui.label("Регулярні вирази: error|warning — один із варіантів; ^TODO — початок рядка; \\d+ — число. Пошук відбувається окремо в кожному рядку. Backreference і look-around не підтримуються.");ui.label("Авто: UTF-8; UTF-16 із BOM; інакше спроба Windows-1251. Для UTF-16 без BOM оберіть LE/BE вручну. Автовизначення старих кодувань не є безпомилковим.");ui.label("Фільтр: txt rs *.log .env; * — будь-яке розширення. Двійкові файли з нульовими байтами пропускаються. Символічні посилання не обходяться. Файли лише читаються.");ui.label("Ліміт результатів означає кількість знайдених рядків, а не кількість входжень слова. Скасування перевіряється між файлами й рядками; поточне читання файла має завершитися.");});
        }
    }
}
impl Drop for App {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180., 780.])
            .with_min_inner_size([950., 680.]),
        ..Default::default()
    };
    eframe::run_native(
        "mygrep — Пошук у файлах",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::light());
            Ok(Box::<App>::default())
        }),
    )
}
