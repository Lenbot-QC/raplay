use std::{
    io::{self, BufReader},
    path::PathBuf,
    fs::{File, read_dir},
    time::Duration,
    error::Error,
    sync::mpsc::channel,
    thread,
};

use ratatui::{
    backend::{Backend, CrosstermBackend},
    crossterm::{
        execute,
        event::{self, Event, KeyCode, KeyEventKind},
        terminal::{
            enable_raw_mode, disable_raw_mode,
            EnterAlternateScreen, LeaveAlternateScreen
        },
    },
    terminal::{Frame, Terminal},
    layout::Rect,
    widgets::{Block, Paragraph}
};
use rodio::{Decoder, OutputStream, Sink, Source};

fn main() -> Result<(), Box<dyn Error>> {
    enable_raw_mode()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;

    let app = App::new();
    let _ = run_app(&mut terminal, app);

    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
    terminal.show_cursor()?;
    Ok(())
}

// P键作为自锁开关控制播放或暂停，暂停模式下长按R键将播放进度重置
// L键控制播放模式：仅顺序播放一次，列表循环，单曲循环，列表循环且随机播放

enum PlayState {Play(bool), Pause(bool), Restart}
enum PlayMode {ListOnce, LoopAll, LoopOne, LoopRnd}

struct AudioFileList {
    dirs: Vec<String>,
    files: Vec<String>,
}

// App目前是必须依赖AudioFileList，以后有可能会抽时间做这块的优化。
impl AudioFileList {
    fn new() -> Self {
        Self {
            dirs: vec![],
            files: vec![],
        }
    }
    fn insert_file(&mut self, file_name: String) {
        self.files.push(file_name);
    }
    fn reset(&mut self) {
        self.files.clear();
    }
}

struct App {
    audio_path: String,             // 当前播放的音频文件路径，初始化为空
    audio_sink: Sink,               // 当前播放的音频文件容器
    song_name: String,              // 当前播放的音频文件名称，初始化为空
    song_curr_time: String,         // 当前播放的音频文件实时时间，初始化为空
    song_duration: String,          // 当前播放的音频文件总时长，初始化为空
    song_progress: String,          // 当前播放的音频文件实时进度
    play_state: PlayState,          // 播放状态
    audio_file_list: AudioFileList, // 用来获取文件位置
    curr_folderpath: PathBuf,       // 当前的播放列表的文件夹路径，初始化为程序目录
    curr_playlist: Vec<String>,     // 当前的播放列表，含所有推测的音频文件名称，有且至少要有一项是作为上一级目录的接口
    curr_songid: u16,               // 当前播放的音频文件，对应列表的第几个，初始化为0
    curr_songnum: u16,              // 当前的播放列表，含所有推测的音频文件数量，初始化为0
}

// show_song_info: ok!
//      用来显示歌名、歌曲编号、文件夹音频文件数量，其中第二、三个数据可作为一个控件一起显示。
// show_song_curr_time, show_song_duration, show_song_progress: ok!
//      用来显示歌曲的进度，其中第一、二个方法可输出秒数，提供给第三个方法用。
// time_to_seek: ok!
//      用来跳转歌曲的指定时间戳。
// load_folder_path: ok!
//      用来读取指定文件夹，可输出为一个新的struct，包含指定文件夹内的文件夹(vec)、所有音频文件的名称(vec)。

impl App {
    fn new() -> Self {
        Self {
            audio_path: String::new(),
            audio_sink: Sink::try_new(&OutputStream::try_default().unwrap().1).unwrap(),
            song_name: String::new(),
            song_curr_time: String::new(),
            song_duration: String::new(),
            song_progress: String::from("-------------------------"),
            play_state: PlayState::Pause(true),
            audio_file_list: AudioFileList::new(),
            curr_folderpath: std::env::current_dir().unwrap(),
            curr_playlist: vec![String::from("..")],
            curr_songid: 0,
            curr_songnum: 0,
        }
    }
    fn show_song_info(&mut self) {
        self.song_name = self.audio_path.clone();
    }
    fn show_song_curr_time(&mut self) -> Result<u64, Box<dyn Error>>{
        let dur = self.audio_sink.get_pos().as_secs();
        self.song_curr_time = format!("{}:{:02}:{:02}", dur/3600, dur%3600/60, dur%60);
        Ok(dur)
    }
    fn show_song_duration(&mut self) -> Result<u64, Box<dyn Error>>{
        if self.audio_path.is_empty() == false {
            let source = Decoder::new(
                BufReader::new(File::open(self.audio_path.as_str())?)
            )?;
            // let dur = source.total_duration().unwrap().as_secs();
            let dur = source.total_duration();
            let dur = match dur {
                Some(dur) => dur.as_secs(),
                None => 36000,
            };
            if dur < 36000 {
                self.song_duration = format!("{}:{:02}:{:02}", dur/3600, dur%3600/60, dur%60);
            }
            else {
                self.song_duration = "?[-_-#]".to_string();
            }
            Ok(dur)
        }
        else {
            self.song_duration.clear();
            Ok(1)
        }
    }
    fn show_song_progress(&mut self, curr: u64, dur: u64) {
        if dur == 36000 {
            self.song_progress = "============/============".to_string();
            return;
        }
        let length = 25;
        let progress = curr * length / dur;
        if !self.audio_sink.empty() {
            self.song_progress.clear();
            for _ in 0..progress {self.song_progress.push('=');}
            self.song_progress.push('>');
            for _ in 0..(length-progress) {self.song_progress.push('-');}
        }
    }
    fn time_to_seek(&mut self, msec: u64) -> Result<(), Box<dyn Error>> {
        self.audio_sink.try_seek(Duration::from_millis(msec))?;
        Ok(())
    }
    fn load_file_path(&mut self, path: PathBuf) -> Result<(), Box<dyn Error>> {
        self.audio_file_list.reset();
        for item in read_dir(path)? {
            let i = item?;
            let n = i.file_name().into_string().unwrap();
            if i.file_type()?.is_file() && n.ends_with("mp3") || n.ends_with("flac") || n.ends_with("ogg") || n.ends_with("wav") {
                self.audio_file_list.insert_file(n);
            }
        }
        Ok(())
    }
}

fn ui(f: &mut Frame, app: &App) {
    f.render_widget(
        Block::bordered(),
        Rect {x: 0, y: 0, width: 45, height: 6}
    );  // 主界面

    f.render_widget(
        Paragraph::new("(P)Play/Pause (Q)Quit"),
        Rect {x: 2, y: 4, width: 41, height: 1}
    );  // 操作简易说明

    f.render_widget(
        Paragraph::new(app.song_name.clone()).centered(),
        Rect {x: 2, y: 1,width: 41, height: 1}
    );  // 显示歌名

    f.render_widget(
        Paragraph::new(app.song_curr_time.clone()),
        Rect {
            x: 2,
            y: 3,
            width: app.song_curr_time.len() as u16,
            height: 1
        }
    );  // 显示歌曲当前时间戳

    f.render_widget(
        Paragraph::new(app.song_duration.clone()),
        Rect {
            x: 43 - app.song_duration.len() as u16,
            y: 3,
            width: app.song_duration.len() as u16,
            height: 1
        }
    );  // 显示歌曲当前总时长

    f.render_widget(
        Paragraph::new(app.song_progress.clone()),
        Rect {x: 10, y: 3, width: 25, height: 1}
    );  // 显示歌曲进度

    f.render_widget(
        Paragraph::new(format!("----kbps {:03}/{:03}", app.curr_songid, app.curr_songnum)),
        Rect {x: 27, y: 4, width: 16, height: 1}
    );  // 显示码率和播放情况

    f.render_widget(
        // Paragraph::new("⇒ ↻ ① ✈ A → B"),
        Paragraph::new("- L - - ---"),
        Rect {x: 2, y: 2, width: 41, height: 1}
    );  // 显示播放模式（部分为UTF-8图标）
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, mut app: App) -> Result<(), Box<dyn Error>> {
    let (tx, rx) = channel();
    let (_stream, stream_handle) = OutputStream::try_default()?;
    app.audio_sink = Sink::try_new(&stream_handle)?;
    loop {
        terminal.draw(|f| ui(f, &app))?;
        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => {
                        tx.send(())?;
                        return Ok(());
                    },
                    KeyCode::Char('p') if app.curr_songid != 0 => {
                        match app.play_state {
                            PlayState::Pause(false) | PlayState::Play(true) if key.kind == KeyEventKind::Press => {
                                app.audio_sink.play();
                                if rx.try_recv().is_ok() {
                                    app.audio_sink.sleep_until_end();
                                    app.audio_sink.clear();
                                }
                                app.play_state = PlayState::Play(false);
                            },
                            PlayState::Play(false) | PlayState::Pause(true) if key.kind == KeyEventKind::Press => {
                                app.audio_sink.pause();
                                app.play_state = PlayState::Pause(false);
                            },
                            _ => {}
                        }
                        
                    },
                    KeyCode::Char('l') => {
                        let _ = app.load_file_path(app.curr_folderpath.clone());
                        app.curr_songnum = app.audio_file_list.files.len() as u16;
                        if app.curr_songnum != 0 {
                            app.curr_songid = 1;
                            app.curr_playlist = app.audio_file_list.files.clone();
                            app.audio_path = app.curr_playlist[(app.curr_songid - 1) as usize].clone();
                        }
                        else {
                            app.curr_songid = 0;
                            app.song_name = String::from("there's no audio files.")
                        }
                    }
                    _ => {}
                }
            }
        }
        else {
            if app.audio_sink.is_paused() == false {
                let curr = app.show_song_curr_time()?;
                let dur = app.show_song_duration()?;
                app.show_song_progress(curr, dur);
                if app.audio_sink.empty() && app.curr_songid != 0 {
                    match app.play_state {
                        PlayState::Pause(true) => {
                            app.audio_sink.append(Decoder::new(
                                BufReader::new(File::open(app.audio_path.clone())?)
                            )?);
                            app.audio_sink.pause();
                            app.play_state = PlayState::Pause(false)
                        },
                        PlayState::Play(false) => {
                            app.curr_songid += 1;
                            if app.curr_songid > app.curr_songnum {
                                app.curr_songid = 1;
                            }
                            app.audio_path = app.curr_playlist[(app.curr_songid - 1) as usize].clone();
                            app.audio_sink.append(Decoder::new(
                                BufReader::new(File::open(app.audio_path.clone())?)
                            )?);
                        }
                        _ => {}
                    }
                    app.show_song_info();
                }
            }
            else {
                thread::sleep(Duration::from_millis(300));
            }
        }
    }
}
