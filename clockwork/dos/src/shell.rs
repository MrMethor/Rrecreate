//! Ported from DOSBox Staging `src/shell/` (GPL-2.0-or-later): COMMAND.COM, the DOS shell, for
//! programs that run one (C's `system()`, a "DOS shell" menu item, an installer's batch files)
//! and for batch files started directly. From DOSBox: the batch reader and its `%` expansion
//! (`shell_batch.cpp`), redirection, pipes and the program search (`shell.cpp`,
//! `shell_misc.cpp`), and the internal commands and their messages (`shell_cmds.cpp`).
//!
//! Unlike DOSBox's, this shell doesn't drive the CPU from the host. It is a small program
//! ([`image`]: the host's step, an `INT 21h`, a jump back) and each step does one command: an
//! internal one in the host, through the DOS services (so files, handles and errors are DOS's
//! own), then a harmless `INT 21h` so that emulated time passes even in a batch loop; an
//! external program as an `INT 21h` EXEC from the shell; waiting for a key or a line as the
//! `INT 21h` that reads it. Its state is plain data in [`crate::dos::DosState`].

use std::collections::VecDeque;

use crate::Machine;
use crate::bios::callback;
use crate::dos;

// The shell's segment (its PSP's): the program at 100h, then its buffers.
const PATH: u16 = 0x0200;
const TAIL: u16 = 0x0280;
const BLOCK: u16 = 0x0300;
const FCB1: u16 = 0x0340;
const FCB2: u16 = 0x0360;
const LINE: u16 = 0x0380;
const SCRATCH: u16 = 0x0400;
const SCRATCH2: u16 = 0x0500;
const DTA: u16 = 0x0580;
const BUFFER: u16 = 0x0600;
const BUFFER_SIZE: u16 = 0x0400;
/// The shell keeps 4 KB of memory (the rest is for the programs it runs), its stack on top.
const PARAGRAPHS: u16 = 0x0100;
const STACK_TOP: u16 = 0x0FFE;
/// Room for environment strings, as `COMMAND /E:1024`.
const ENVIRONMENT_BYTES: u16 = 1024;

const CF: u32 = 0x0001;

/// COMMAND.COM: the host's step, the `INT 21h` it set up, again.
pub(crate) fn image() -> Vec<u8> {
    let mut code = crate::bios::callback_instruction(callback::SHELL).to_vec();
    code.extend([0xCD, 0x21, 0xEB, 0xF8]);
    code
}

/// A running shell.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Shell {
    pub(crate) psp: u16,
    echo: bool,
    /// `/C`: run this, then exit (taken when it runs).
    command: Option<String>,
    /// No `/C`: read commands at a prompt.
    interactive: bool,
    /// `/P`: EXIT doesn't end it.
    permanent: bool,
    /// Commands to run before reading more: a FOR's, a pipe's stages.
    queue: VecDeque<Pending>,
    batches: Vec<Batch>,
    waiting: Waiting,
    /// Redirections to undo when the command running ends, a line's at a time (CALL and IF
    /// run a line inside one).
    restore: Vec<Restore>,
    errorlevel: u8,
    pipes: u32,
    /// Running a batch file with CALL: the caller goes on afterwards.
    calling: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Pending {
    line: String,
    /// A pipe's stage reads what the one before wrote.
    stdin: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Batch {
    /// `%0` (the name as entered), then the arguments.
    args: Vec<String>,
    shift: usize,
    text: Vec<u8>,
    pos: usize,
    echo: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Waiting {
    Nothing,
    Program,
    Line,
    Pause,
    Choice { choices: String, default: Option<(char, u64)> },
}

/// The shell's own standard input and output, saved while a command's are redirected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Restore {
    stdin: Option<u16>,
    stdout: Option<u16>,
}

/// What the stub's `INT 21h` does next.
enum Next {
    /// Nothing much (get the current drive): the next step comes right after.
    Yield,
    Exec,
    ReadLine,
    ReadKey,
    KeyStatus,
    Exit,
}

/// The host's part of COMMAND.COM: finishes what the last `INT 21h` was for, runs the next
/// command, and sets up the next `INT 21h`.
pub(crate) fn step(m: &mut Machine) {
    let psp = m.dos.psp;
    let mut shell = match m.dos.shells.iter().position(|s| s.psp == psp) {
        Some(i) => m.dos.shells.remove(i),
        None => Shell::start(m, psp),
    };
    let next = match shell.resume(m) {
        Some(next) => next,
        None => shell.next(m),
    };
    let r = &mut m.cpu.regs;
    r.seg[cpu::seg::DS] = psp;
    r.seg[cpu::seg::ES] = psp;
    r.flags &= !CF;
    match next {
        Next::Yield => r.set_ax(0x1900),
        Next::Exec => {
            r.set_ax(0x4B00);
            r.set_dx(PATH);
            r.set_bx(BLOCK);
        }
        Next::ReadLine => {
            m.mem.write_u8(Machine::lin(psp, LINE), 127);
            let r = &mut m.cpu.regs;
            r.set_ax(0x0A00);
            r.set_dx(LINE);
        }
        Next::ReadKey => r.set_ax(0x0800),
        Next::KeyStatus => r.set_ax(0x0B00),
        Next::Exit => {
            r.set_ax(0x4C00);
            return;
        }
    }
    m.dos.shells.push(shell);
}

/// Latin-1 both ways: DOS text is bytes, kept as `char`s one to one.
fn text(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

fn bytes(text: &str) -> Vec<u8> {
    text.chars().map(|c| c as u32 as u8).collect()
}

impl Shell {
    fn start(m: &mut Machine, psp: u16) -> Shell {
        // Keep 4 KB; the stack moves into them.
        let _ = dos::resize(m, psp, PARAGRAPHS);
        m.cpu.regs.set_sp(STACK_TOP);
        give_environment_room(m, psp);
        let tail_length = usize::from(m.mem.read_u8(Machine::lin(psp, 0x80)).min(126));
        let tail =
            text(&(0..tail_length).map(|i| m.mem.read_u8(Machine::lin(psp, 0x81 + i as u16))).collect::<Vec<_>>());
        let mut shell = Shell {
            psp,
            echo: true,
            command: None,
            interactive: true,
            permanent: false,
            queue: VecDeque::new(),
            batches: Vec::new(),
            waiting: Waiting::Nothing,
            restore: Vec::new(),
            errorlevel: 0,
            pipes: 0,
            calling: false,
        };
        // Switches until /C or /K, whose rest is the command (`/Cdir` too).
        let mut rest = tail.trim_start();
        while let Some(switch) = rest.strip_prefix('/') {
            let (word, after) = switch.split_at(switch.find([' ', '\t', '/']).unwrap_or(switch.len()));
            let letter = word.chars().next().map(|c| c.to_ascii_uppercase());
            if matches!(letter, Some('C' | 'K')) {
                let command = format!("{}{after}", &word[1..]);
                let command = command.trim_start().split(['\r', '\n']).next().unwrap_or("").to_string();
                if letter == Some('C') {
                    shell.interactive = false;
                    shell.command = Some(command);
                } else {
                    shell.queue.push_back(Pending { line: command, stdin: None });
                }
                return shell;
            }
            if word.eq_ignore_ascii_case("P") {
                shell.permanent = true;
            }
            // /E:n, /F, /MSG ...: the environment is 1 KB here either way.
            rest = after.trim_start();
        }
        shell
    }

    /// Finishes what the last `INT 21h` was for; Some: what to do instead of the next command.
    fn resume(&mut self, m: &mut Machine) -> Option<Next> {
        match std::mem::replace(&mut self.waiting, Waiting::Nothing) {
            Waiting::Nothing => None,
            Waiting::Program => {
                if m.cpu.regs.flags & CF != 0 {
                    let message = match m.cpu.regs.ax() {
                        2 | 3 => "Bad command or file name",
                        5 => "Access denied",
                        8 => "Program too big to fit in memory",
                        11 => "Invalid format",
                        _ => "Cannot execute the program",
                    };
                    self.print(m, &format!("{message}\r\n"));
                    self.errorlevel = 0;
                } else {
                    self.errorlevel = m.dos.return_code as u8;
                }
                self.unredirect(m);
                None
            }
            Waiting::Line => {
                let at = Machine::lin(self.psp, LINE);
                let length = m.mem.read_u8(at + 1);
                let line = text(&(0..u32::from(length)).map(|i| m.mem.read_u8(at + 2 + i)).collect::<Vec<_>>());
                self.print(m, "\n");
                self.queue.push_back(Pending { line, stdin: None });
                None
            }
            Waiting::Pause => {
                if m.cpu.regs.al() == 0 {
                    // An extended key: its second byte.
                    self.waiting = Waiting::Pause;
                    return Some(Next::ReadKey);
                }
                self.print(m, "\r\n");
                self.unredirect(m);
                None
            }
            Waiting::Choice { choices, default: Some((choice, deadline)) } => {
                // Polling (AH=0Bh): a key is ready, or the time is up and the default is taken.
                if m.cpu.regs.al() == 0xFF {
                    self.waiting = Waiting::Choice { choices, default: None };
                    return Some(Next::ReadKey);
                }
                if m.cpu.cycles >= deadline {
                    self.chose(m, &choices, choice);
                    return None;
                }
                self.waiting = Waiting::Choice { choices, default: Some((choice, deadline)) };
                Some(Next::KeyStatus)
            }
            Waiting::Choice { choices, default: None } => {
                let key = char::from(m.cpu.regs.al()).to_ascii_uppercase();
                if key == '\u{3}' {
                    self.print(m, "\r\n");
                    self.errorlevel = 0;
                    self.unredirect(m);
                    return None;
                }
                if choices.contains(key) {
                    self.chose(m, &choices, key);
                    return None;
                }
                self.waiting = Waiting::Choice { choices, default: None };
                Some(Next::ReadKey)
            }
        }
    }

    fn chose(&mut self, m: &mut Machine, choices: &str, choice: char) {
        self.print(m, &format!("{choice}\r\n"));
        self.errorlevel = choices.find(choice).map_or(0, |i| (i % 254 + 1) as u8);
        self.unredirect(m);
    }

    /// Runs the next command, from the queue, the batch file running, the `/C` command or the
    /// prompt.
    fn next(&mut self, m: &mut Machine) -> Next {
        if let Some(pending) = self.queue.pop_front() {
            return self.line(m, &pending.line, pending.stdin.as_deref());
        }
        if let Some(batch) = self.batches.last_mut() {
            let Some(line) = batch.read_line() else {
                self.batches.pop();
                return Next::Yield;
            };
            let line = self.expand(m, &line);
            if self.batches.last().is_some_and(|b| b.echo) && !line.starts_with('@') {
                let prompt = self.prompt(m);
                self.print(m, &format!("{prompt}{line}\r\n"));
            }
            return self.line(m, &line, None);
        }
        if let Some(command) = self.command.take() {
            return self.line(m, &command, None);
        }
        if !self.interactive {
            return Next::Exit;
        }
        if self.echo {
            let prompt = self.prompt(m);
            self.print(m, &prompt);
        }
        self.waiting = Waiting::Line;
        Next::ReadLine
    }

    /// One line: its pipe, its redirections, then the command (DOSBox `ParseLine`).
    fn line(&mut self, m: &mut Machine, line: &str, stdin: Option<&str>) -> Next {
        let line = line.strip_prefix('@').unwrap_or(line).trim();
        if line.is_empty() {
            return Next::Yield;
        }
        if let Some(bar) = outside_quotes(line).find(|&(_, c)| c == '|').map(|(i, _)| i) {
            // Through a temporary file: the first stage's output is the next one's input.
            self.pipes += 1;
            let file = format!("PIPE{:04}.TMP", self.pipes % 10000);
            let first = format!("{} >{file}", &line[..bar]);
            let mut stages = vec![
                Pending { line: first, stdin: stdin.map(String::from) },
                Pending { line: line[bar + 1..].to_string(), stdin: Some(file.clone()) },
                Pending { line: format!("DEL {file}"), stdin: None },
            ];
            while let Some(stage) = stages.pop() {
                self.queue.push_front(stage);
            }
            return Next::Yield;
        }
        let Some(r) = redirections(line) else {
            self.print(m, "Syntax error\r\n");
            return Next::Yield;
        };
        if let Err(message) = self.redirect(m, r.stdin.as_deref().or(stdin), r.stdout.as_ref()) {
            self.print(m, &message);
            return Next::Yield;
        }
        let next = self.command(m, &r.line);
        // A program, or a wait for a key: undone when it ends (resume).
        if matches!(next, Next::Yield | Next::Exit) {
            self.unredirect_one(m);
        }
        next
    }

    /// A command: internal, a drive change, or a program (DOSBox `DoCommand`).
    fn command(&mut self, m: &mut Machine, line: &str) -> Next {
        let line = line.trim();
        let mut end = 0;
        for (i, c) in line.char_indices() {
            if matches!(c, ' ' | '/' | '\t' | '=' | '"') {
                break;
            }
            // `cd..`, `cd\games`, `echo.`: a name followed by . or \ is the command if internal.
            if matches!(c, '.' | '\\') && is_internal(&line[..i]) {
                break;
            }
            end = i + c.len_utf8();
        }
        let (name, args) = line.split_at(end);
        if name.is_empty() {
            return Next::Yield;
        }
        if let Some(next) = self.internal(m, name, args) {
            return next;
        }
        if name.len() == 2 && name.ends_with(':') || name.len() == 3 && name.ends_with(":\\") {
            let letter = name.as_bytes()[0].to_ascii_uppercase();
            if letter.is_ascii_uppercase() {
                let drive = letter - b'A';
                if m.dos.drive_exists(drive) {
                    m.dos.drive = drive;
                } else {
                    self.print(m, "Invalid drive specification\r\n");
                }
                return Next::Yield;
            }
        }
        match self.program(m, name, args) {
            Some(next) => next,
            None => {
                self.print(m, "Bad command or file name\r\n");
                Next::Yield
            }
        }
    }

    /// An external program or batch file, found the way COMMAND.COM looks (DOSBox `ResolvePath`):
    /// the name as given, then each PATH folder; without an extension .COM, .EXE and .BAT.
    fn program(&mut self, m: &mut Machine, name: &str, args: &str) -> Option<Next> {
        let path = self.resolve(m, name)?;
        if path.to_ascii_uppercase().ends_with(".BAT") {
            let echo = self.batches.last().map_or(self.echo, |b| b.echo);
            if !self.calling {
                self.batches.pop();
            }
            let Some(text) = self.read_file(m, &path) else {
                self.print(m, &format!("Could not open {path}\r\n"));
                return Some(Next::Yield);
            };
            let mut list = vec![name.to_string()];
            list.extend(args.split([' ', '\t', ',', ';', '=']).filter(|a| !a.is_empty()).map(String::from));
            self.batches.push(Batch { args: list, shift: 0, text, pos: 0, echo });
            return Some(Next::Yield);
        }
        // EXEC: the path, the command tail, the first two arguments parsed into FCBs.
        let psp = self.psp;
        let mut asciiz = bytes(&path);
        asciiz.push(0);
        m.mem.load(Machine::lin(psp, PATH), &asciiz);
        let mut tail = bytes(args);
        tail.truncate(126);
        let mut block = vec![tail.len() as u8];
        block.extend(&tail);
        block.push(0x0D);
        m.mem.load(Machine::lin(psp, TAIL), &block);
        let mut words = args.split(['=', ';', ',', '\t', ' ']).filter(|w| !w.is_empty());
        for fcb in [FCB1, FCB2] {
            let mut word = bytes(words.next().unwrap_or(""));
            word.push(0);
            m.mem.load(Machine::lin(psp, SCRATCH), &word);
            dos::call_service(m, callback::DOS, |r| {
                r.set_ax(0x2901);
                r.seg[cpu::seg::DS] = psp;
                r.set_si(SCRATCH);
                r.seg[cpu::seg::ES] = psp;
                r.set_di(fcb);
            });
        }
        let at = Machine::lin(psp, BLOCK);
        m.mem.write_u16(at, 0); // a copy of the shell's environment
        for (i, offset) in [TAIL, FCB1, FCB2].into_iter().enumerate() {
            m.mem.write_u16(at + 2 + 4 * i as u32, offset);
            m.mem.write_u16(at + 4 + 4 * i as u32, psp);
        }
        self.waiting = Waiting::Program;
        Some(Next::Exec)
    }

    fn resolve(&self, m: &mut Machine, name: &str) -> Option<String> {
        let mut prefixes = vec![String::new()];
        if let Some(path) = self.variable(m, "PATH") {
            prefixes.extend(
                path.split(';')
                    .filter(|d| !d.is_empty())
                    .map(|d| if d.ends_with('\\') { d.into() } else { format!("{d}\\") }),
            );
        }
        let has_extension = name.len() > 4 && name.as_bytes()[name.len() - 4] == b'.';
        for prefix in &prefixes {
            let candidates: Vec<String> = if has_extension {
                vec![format!("{prefix}{name}")]
            } else {
                [".COM", ".EXE", ".BAT"].iter().map(|e| format!("{prefix}{name}{e}")).collect()
            };
            if let Some(found) = candidates.into_iter().find(|c| self.is_file(m, c)) {
                return Some(found);
            }
        }
        None
    }

    /// Internal commands (DOSBox `shell_cmds.cpp`); None: not one.
    fn internal(&mut self, m: &mut Machine, name: &str, args: &str) -> Option<Next> {
        let upper = name.to_ascii_uppercase();
        let next = match upper.as_str() {
            "REM" => Next::Yield,
            "ECHO" => {
                self.echo_command(m, args);
                Next::Yield
            }
            "CLS" => {
                dos::call_service(m, callback::VIDEO, |r| r.set_ax(0x0003));
                Next::Yield
            }
            "PAUSE" => {
                self.print(m, "Press any key to continue . . . ");
                self.waiting = Waiting::Pause;
                return Some(Next::ReadKey);
            }
            "SET" => {
                self.set_command(m, args);
                Next::Yield
            }
            "PATH" => {
                let value = args.trim_start_matches([' ', '\t', '=']).trim_end();
                if value.is_empty() {
                    let text = self.variable(m, "PATH").map_or("No Path".into(), |p| format!("PATH={p}"));
                    self.print(m, &format!("{text}\r\n"));
                } else {
                    let value = if value == ";" { String::new() } else { value.to_ascii_uppercase() };
                    self.set_variable(m, "PATH", &value);
                }
                Next::Yield
            }
            "PROMPT" => {
                self.set_variable(m, "PROMPT", args.trim_start_matches([' ', '\t', '=']).trim_end());
                Next::Yield
            }
            "CD" | "CHDIR" => {
                self.cd(m, args.trim());
                Next::Yield
            }
            "MD" | "MKDIR" => {
                if !self.path_call(m, 0x39, args.trim()) {
                    self.print(m, "Unable to create directory\r\n");
                }
                Next::Yield
            }
            "RD" | "RMDIR" => {
                if !self.path_call(m, 0x3A, args.trim()) {
                    self.print(m, "Invalid path, not directory,\r\nor directory not empty\r\n");
                }
                Next::Yield
            }
            "DEL" | "ERASE" => {
                self.delete(m, args);
                Next::Yield
            }
            "REN" | "RENAME" => {
                self.rename(m, args);
                Next::Yield
            }
            "COPY" => {
                self.copy(m, args);
                Next::Yield
            }
            "TYPE" => {
                self.type_command(m, args);
                Next::Yield
            }
            "DIR" => {
                self.dir(m, args);
                Next::Yield
            }
            "DATE" => {
                self.date(m, args);
                Next::Yield
            }
            "TIME" => {
                self.time(m, args);
                Next::Yield
            }
            "ATTRIB" => {
                self.attrib(m, args);
                Next::Yield
            }
            "VER" => {
                self.print(m, "\r\nMS-DOS Version 5.00 (Clockwork)\r\n\r\n");
                Next::Yield
            }
            "EXIT" => {
                if self.permanent {
                    Next::Yield
                } else {
                    self.batches.clear();
                    self.queue.clear();
                    Next::Exit
                }
            }
            "IF" => return Some(self.if_command(m, args)),
            "GOTO" => {
                self.goto(m, args);
                Next::Yield
            }
            "SHIFT" => {
                if let Some(batch) = self.batches.last_mut() {
                    batch.shift += 1;
                }
                Next::Yield
            }
            "CALL" => {
                self.calling = true;
                let next = self.line(m, args, None);
                self.calling = false;
                return Some(next);
            }
            "FOR" => {
                self.for_command(m, args);
                Next::Yield
            }
            "CHOICE" => return Some(self.choice(m, args)),
            "LH" | "LOADHIGH" => return Some(self.command(m, args)),
            "CTTY" | "BREAK" | "VERIFY" => Next::Yield,
            _ => return None,
        };
        Some(next)
    }

    /// DATE [/T] [MM-DD-YYYY] (DOSBox `CMD_DATE`, the USA format): shows or sets the DOS date.
    fn date(&mut self, m: &mut Machine, args: &str) {
        let words: Vec<&str> = args.split_whitespace().collect();
        if words.iter().any(|w| w.eq_ignore_ascii_case("/H")) {
            m.diagnostic("DATE /H: the clock is virtual (clockwork's start time), not the host's".into());
            return;
        }
        let only = words.iter().any(|w| w.eq_ignore_ascii_case("/T"));
        if let Some(date) = words.iter().find(|w| !w.starts_with('/')) {
            let parts: Vec<Option<u16>> = date.split('-').map(|p| p.parse().ok()).collect();
            if let [Some(month), Some(day), Some(year)] = parts[..] {
                let (r, _) = dos::call_service(m, callback::DOS, |r| {
                    r.set_ah(0x2B);
                    r.set_cx(year);
                    r.set_dh(month as u8);
                    r.set_dl(day as u8);
                });
                if r.al() == 0xFF || month > 0xFF || day > 0xFF {
                    self.print(m, "The specified date is not correct.\r\n");
                }
                return;
            }
        }
        let (r, _) = dos::call_service(m, callback::DOS, |r| r.set_ah(0x2A));
        let date = format!("{:02}-{:02}-{:04}", r.dh(), r.dl(), r.cx());
        if only {
            self.print(m, &format!("{date}\r\n"));
        } else {
            let day = &"SunMonTueWedThuFriSat"[usize::from(r.al() % 7) * 3..][..3];
            self.print(
                m,
                &format!("Current date: {day} {date}\r\nRun date MM-DD-YYYY to change the current date.\r\n"),
            );
        }
    }

    /// TIME [/T] [hh:mm:ss] (DOSBox `CMD_TIME`, the USA format): shows or sets the DOS time.
    fn time(&mut self, m: &mut Machine, args: &str) {
        let words: Vec<&str> = args.split_whitespace().collect();
        if words.iter().any(|w| w.eq_ignore_ascii_case("/H")) {
            m.diagnostic("TIME /H: the clock is virtual (clockwork's start time), not the host's".into());
            return;
        }
        let only = words.iter().any(|w| w.eq_ignore_ascii_case("/T"));
        if let Some(time) = words.iter().find(|w| !w.starts_with('/')) {
            let parts: Vec<Option<u8>> = time.split(':').map(|p| p.parse().ok()).collect();
            if let [Some(hour), Some(minute), Some(second)] = parts[..] {
                let (r, _) = dos::call_service(m, callback::DOS, |r| {
                    r.set_ah(0x2D);
                    r.set_ch(hour);
                    r.set_cl(minute);
                    r.set_dh(second);
                    r.set_dl(0);
                });
                if r.al() == 0xFF {
                    self.print(m, "The specified time is not correct.\r\n");
                }
                return;
            }
        }
        let (r, _) = dos::call_service(m, callback::DOS, |r| r.set_ah(0x2C));
        let (hour, minute, second, hundredths) = (r.ch(), r.cl(), r.dh(), r.dl());
        if only {
            self.print(m, &format!("{hour}:{minute:02}:{second:02}\r\n"));
        } else {
            // The 12-hour clock of the USA: 1:02:03.04pm.
            let shown = if hour == 12 { 12 } else { hour % 12 };
            let half = if hour != 12 && hour == shown { "am" } else { "pm" };
            self.print(
                m,
                &format!(
                    "Current time: {shown}:{minute:02}:{second:02}.{hundredths:02}{half}\r\n\
                     Run time hh:mm:ss to change the current time.\r\n"
                ),
            );
        }
    }

    /// ATTRIB [+|-A] [+|-S] [+|-H] [+|-R] [pattern] [/S] (DOSBox `CMD_ATTRIB`): shows or changes
    /// file attributes, in subdirectories too with /S.
    fn attrib(&mut self, m: &mut Machine, args: &str) {
        let mut pattern = "*.*".to_string();
        let (mut set, mut clear, mut recurse) = (0u8, 0u8, false);
        for word in args.split_whitespace() {
            let bit = |c: &str| match c.to_ascii_uppercase().as_str() {
                "A" => Some(0x20),
                "S" => Some(0x04),
                "H" => Some(0x02),
                "R" => Some(0x01),
                _ => None,
            };
            match (word.get(..1), word.get(1..).and_then(bit)) {
                (Some("+"), Some(b)) => set |= b,
                (Some("-"), Some(b)) => clear |= b,
                _ if word.eq_ignore_ascii_case("/S") => recurse = true,
                (Some("/"), _) => {
                    self.print(m, &format!("Illegal switch: {word}\r\n"));
                    return;
                }
                _ => pattern = word.to_string(),
            }
        }
        let (dir, name) = split_dir(&pattern);
        let (dir, name) = (dir.to_string(), if name.is_empty() { "*.*".to_string() } else { name.to_string() });
        let mut dirs = vec![dir];
        let mut found = false;
        while let Some(dir) = dirs.pop() {
            for file in self.find(m, &format!("{dir}{name}"), 0x37) {
                if file.attr & 0x10 != 0 && name.contains(['*', '?']) {
                    continue;
                }
                found = true;
                let path = format!("{dir}{}", file.name);
                let full = self.truename(m, &path).unwrap_or(path.clone());
                let mut attr = file.attr & 0x27;
                if set | clear != 0 {
                    attr = (attr | set) & !clear;
                    if !self.set_attributes(m, &path, attr) {
                        self.print(m, &format!("Unable to set attributes: {full}\r\n"));
                        continue;
                    }
                }
                let flag = |bit: u8, c: char| if attr & bit != 0 { c } else { ' ' };
                let line = format!(
                    "  {}  {}{}{}     {full}\r\n",
                    flag(0x20, 'A'),
                    flag(0x02, 'H'),
                    flag(0x04, 'S'),
                    flag(0x01, 'R')
                );
                self.print(m, &line);
            }
            if recurse {
                let mut subdirs: Vec<String> = self
                    .find(m, &format!("{dir}*.*"), 0x16)
                    .into_iter()
                    .filter(|f| f.attr & 0x10 != 0)
                    .map(|f| format!("{dir}{}\\", f.name))
                    .collect();
                subdirs.reverse();
                dirs.extend(subdirs);
            }
        }
        if !found {
            self.print(m, &format!("File not found - '{pattern}'\r\n"));
        }
    }

    /// TRUENAME (AH=60h): a path in full.
    fn truename(&self, m: &mut Machine, path: &str) -> Option<String> {
        self.put_path(m, SCRATCH, path);
        let psp = self.psp;
        let (_, failed) = dos::call_service(m, callback::DOS, |r| {
            r.set_ah(0x60);
            r.seg[cpu::seg::DS] = psp;
            r.set_si(SCRATCH);
            r.seg[cpu::seg::ES] = psp;
            r.set_di(SCRATCH2);
        });
        let at = Machine::lin(psp, SCRATCH2);
        (!failed).then(|| text(&(0..128).map(|i| m.mem.read_u8(at + i)).take_while(|&b| b != 0).collect::<Vec<_>>()))
    }

    /// AX=4301h: sets a file's attributes.
    fn set_attributes(&self, m: &mut Machine, path: &str, attr: u8) -> bool {
        self.put_path(m, SCRATCH, path);
        let psp = self.psp;
        let (_, failed) = dos::call_service(m, callback::DOS, |r| {
            r.set_ax(0x4301);
            r.set_cx(u16::from(attr));
            r.seg[cpu::seg::DS] = psp;
            r.set_dx(SCRATCH);
        });
        !failed
    }

    fn echo_command(&mut self, m: &mut Machine, args: &str) {
        let echo = self.batches.last().map_or(self.echo, |b| b.echo);
        if args.is_empty() {
            self.print(m, if echo { "ECHO is on\r\n" } else { "ECHO is off\r\n" });
            return;
        }
        let word = args.trim();
        if word.eq_ignore_ascii_case("OFF") || word.eq_ignore_ascii_case("ON") {
            let on = word.eq_ignore_ascii_case("ON");
            match self.batches.last_mut() {
                Some(batch) => batch.echo = on,
                None => self.echo = on,
            }
            return;
        }
        // The character after ECHO (a space, a dot, a slash) isn't printed.
        let rest: String = args.chars().skip(1).collect();
        self.print(m, &format!("{rest}\r\n"));
    }

    fn set_command(&mut self, m: &mut Machine, args: &str) {
        let args = args.trim();
        if args.is_empty() {
            for (name, value) in environment(m, self.psp) {
                self.print(m, &format!("{name}={value}\r\n"));
            }
            return;
        }
        let Some((name, value)) = args.split_once('=') else {
            let text = match self.variable(m, args) {
                Some(value) => format!("{}={value}\r\n", args.to_ascii_uppercase()),
                None => format!("Environment variable {args} not defined\r\n"),
            };
            self.print(m, &text);
            return;
        };
        let value = self.expand_variables(m, value);
        self.set_variable(m, name.trim(), &value);
    }

    fn if_command(&mut self, m: &mut Machine, args: &str) -> Next {
        let mut rest = args.trim_start_matches([' ', '\t', '=']);
        let mut negate = false;
        while starts_with_word(rest, "NOT") && rest[3..].starts_with([' ', '\t', '=']) {
            negate = !negate;
            rest = rest[3..].trim_start_matches([' ', '\t', '=']);
        }
        let condition = if starts_with_word(rest, "ERRORLEVEL") {
            let (number, after) = split_word(rest[10..].trim_start_matches([' ', '\t', '=']));
            let Ok(n) = number.parse::<u16>() else {
                self.print(m, "IF ERRORLEVEL: Missing number\r\n");
                return Next::Yield;
            };
            rest = after;
            u16::from(self.errorlevel) >= n
        } else if starts_with_word(rest, "EXIST ") {
            let (file, after) = split_word(rest[6..].trim_start());
            if file.is_empty() {
                self.print(m, "IF EXIST: Missing filename\r\n");
                return Next::Yield;
            }
            rest = after;
            !self.find(m, file, 0x37).is_empty()
        } else {
            // string1==string2
            let Some(equals) = rest.find("==") else {
                self.print(m, "Syntax error\r\n");
                return Next::Yield;
            };
            let first = rest[..equals].trim_end();
            let first = first.split([' ', '\t']).next().unwrap_or("");
            let after = rest[equals + 2..].trim_start_matches([' ', '\t', '=']);
            let (second, after) = split_word(after);
            rest = after;
            first == second
        };
        if condition != negate { self.command(m, rest.trim_start_matches([' ', '\t', '='])) } else { Next::Yield }
    }

    fn goto(&mut self, m: &mut Machine, args: &str) {
        let label = args.trim().trim_start_matches(':');
        let label = label.split([' ', '\t']).next().unwrap_or("");
        let Some(batch) = self.batches.last_mut() else { return };
        if label.is_empty() {
            self.print(m, "No label supplied to GOTO command\r\n");
            return;
        }
        if !batch.goto(label) {
            self.print(m, &format!("Label {label} not found\r\n"));
            self.batches.pop();
        }
    }

    fn for_command(&mut self, m: &mut Machine, args: &str) {
        const DELIMITERS: [char; 5] = [',', ';', '=', ' ', '\t'];
        let syntax = |shell: &mut Self, m: &mut Machine| shell.print(m, "Syntax error\r\n");
        let rest = args.trim_start_matches(DELIMITERS);
        let (variable, rest) = rest.split_at(rest.find(DELIMITERS).unwrap_or(rest.len()));
        let rest = rest.trim_start_matches(DELIMITERS);
        let (keyword, rest) = rest.split_at(rest.find(DELIMITERS).unwrap_or(rest.len()));
        let variable = variable.trim_start_matches('%');
        if variable.chars().count() != 1 || !keyword.eq_ignore_ascii_case("IN") {
            return syntax(self, m);
        }
        let (Some(open), Some(close)) = (rest.find('('), rest.find(')')) else { return syntax(self, m) };
        if close < open {
            return syntax(self, m);
        }
        let mut items = Vec::new();
        for item in rest[open + 1..close].split(DELIMITERS).filter(|s| !s.is_empty()) {
            if item.contains(['*', '?']) {
                items.extend(self.find(m, item, 0x21).into_iter().map(|f| f.name));
            } else {
                items.push(item.to_string());
            }
        }
        let after = rest[close + 1..].trim_start_matches(DELIMITERS);
        let (keyword, command) = after.split_at(after.find(DELIMITERS).unwrap_or(after.len()));
        let command = command.trim_start_matches(DELIMITERS);
        if !keyword.eq_ignore_ascii_case("DO") || command.is_empty() || starts_with_word(command, "FOR") {
            return syntax(self, m);
        }
        let placeholder = format!("%{variable}");
        for item in items.iter().rev() {
            self.queue.push_front(Pending { line: command.replace(&placeholder, item), stdin: None });
        }
    }

    /// CHOICE (DOS 6's, as DOSBox has it built in): `/C[:]keys`, `/N`, `/S`, `/T[:]c,nn`.
    fn choice(&mut self, m: &mut Machine, args: &str) -> Next {
        let mut keys = "YN".to_string();
        let mut show = true;
        let mut case_sensitive = false;
        let mut default = None;
        let mut text = Vec::new();
        for word in args.split_whitespace() {
            let upper = word.to_ascii_uppercase();
            if let Some(list) = upper.strip_prefix("/C") {
                keys = word[2..].trim_start_matches(':').to_string();
                if list.is_empty() {
                    keys = "YN".into();
                }
            } else if upper == "/N" {
                show = false;
            } else if upper == "/S" {
                case_sensitive = true;
            } else if let Some(spec) = upper.strip_prefix("/T") {
                let spec = spec.trim_start_matches(':');
                if let Some((c, seconds)) = spec.split_once(',')
                    && let (Some(c), Ok(seconds)) = (c.chars().next(), seconds.parse::<u64>())
                {
                    default = Some((c, seconds));
                }
            } else {
                text.push(word.trim_matches('"'));
            }
        }
        if !case_sensitive {
            keys = keys.to_ascii_uppercase();
        }
        let mut unique = String::new();
        for c in keys.chars() {
            if !unique.contains(c) {
                unique.push(c);
            }
        }
        let mut prompt: String = text.iter().map(|t| format!("{t} ")).collect();
        if show {
            let list: Vec<String> = unique.chars().map(String::from).collect();
            prompt.push_str(&format!("[{}]?", list.join(",")));
        }
        self.print(m, &prompt);
        let default = default
            .filter(|&(c, seconds)| unique.contains(c.to_ascii_uppercase()) && seconds > 0)
            .map(|(c, seconds)| (c.to_ascii_uppercase(), m.cpu.cycles + m.cycle_at_ms(seconds * 1000)));
        let polling = default.is_some();
        self.waiting = Waiting::Choice { choices: unique, default };
        if polling { Next::KeyStatus } else { Next::ReadKey }
    }

    fn cd(&mut self, m: &mut Machine, args: &str) {
        let drive_only = args.len() == 2 && args.ends_with(':');
        if args.is_empty() || drive_only {
            let drive =
                if drive_only { args.as_bytes()[0].to_ascii_uppercase().wrapping_sub(b'A') } else { m.dos.drive };
            if !m.dos.drive_exists(drive) {
                self.print(m, "Invalid drive specification\r\n");
                return;
            }
            let cwd = m.dos.cwd[usize::from(drive)].clone();
            self.print(m, &format!("{}:\\{cwd}\r\n", char::from(b'A' + drive)));
            return;
        }
        if !self.path_call(m, 0x3B, args) {
            self.print(m, "Invalid directory\r\n");
        }
    }

    fn delete(&mut self, m: &mut Machine, args: &str) {
        let pattern = args.split_whitespace().find(|w| !w.starts_with('/')).unwrap_or("");
        if pattern.is_empty() {
            self.print(m, "Required parameter missing\r\n");
            return;
        }
        let (dir, _) = split_dir(pattern);
        let files = self.find(m, pattern, 0x21);
        if files.is_empty() {
            self.print(m, "File not found\r\n");
            return;
        }
        for file in files {
            if !self.path_call(m, 0x41, &format!("{dir}{}", file.name)) {
                self.print(m, &format!("Access denied - {}\r\n", file.name));
            }
        }
    }

    fn rename(&mut self, m: &mut Machine, args: &str) {
        let mut words = args.split_whitespace();
        let (Some(from), Some(to)) = (words.next(), words.next()) else {
            self.print(m, "Required parameter missing\r\n");
            return;
        };
        let (dir, _) = split_dir(from);
        let files = self.find(m, from, 0x21);
        if files.is_empty() {
            self.print(m, "File not found\r\n");
            return;
        }
        for file in files {
            let target = format!("{dir}{}", rename_pattern(&file.name, split_dir(to).1));
            if !self.two_path_call(m, 0x56, &format!("{dir}{}", file.name), &target) {
                self.print(m, "Duplicate file name or file not found\r\n");
            }
        }
    }

    /// COPY sources [destination]: sources joined with + are concatenated; a destination folder
    /// (or none: the current one) takes each file under its name. Switches are ignored.
    fn copy(&mut self, m: &mut Machine, args: &str) {
        let words: Vec<&str> = args.split_whitespace().filter(|w| !w.starts_with('/')).collect();
        let Some(&first) = words.first() else {
            self.print(m, "Required parameter missing\r\n");
            return;
        };
        let destination = words.get(1).copied().unwrap_or("");
        let mut sources = Vec::new();
        for source in first.split('+').filter(|s| !s.is_empty()) {
            let (dir, _) = split_dir(source);
            let found = self.find(m, source, 0x21);
            if found.is_empty() {
                self.print(m, &format!("File not found - {source}\r\n"));
                return;
            }
            sources.extend(found.into_iter().map(|f| (format!("{dir}{}", f.name), f.name)));
        }
        let concatenate = first.contains('+');
        let to_folder = destination.is_empty()
            || destination.ends_with('\\')
            || destination.ends_with(':')
            || self.is_dir(m, destination);
        let mut copied = 0;
        if concatenate || !to_folder {
            let target =
                if to_folder { format!("{}{}", folder(destination), sources[0].1) } else { destination.to_string() };
            if self.copy_files(m, sources.iter().map(|(path, _)| path.as_str()), &target) {
                copied = 1;
            }
        } else {
            for (path, name) in &sources {
                let target = format!("{}{name}", folder(destination));
                if self.copy_files(m, std::iter::once(path.as_str()), &target) {
                    copied += 1;
                }
            }
        }
        self.print(m, &format!("        {copied} file(s) copied\r\n"));
    }

    fn copy_files<'a>(&mut self, m: &mut Machine, sources: impl Iterator<Item = &'a str>, target: &str) -> bool {
        let mut data = Vec::new();
        for source in sources {
            match self.read_file(m, source) {
                Some(bytes) => data.extend(bytes),
                None => return false,
            }
        }
        if self.write_file(m, target, &data) {
            true
        } else {
            self.print(m, "File creation error\r\n");
            false
        }
    }

    fn type_command(&mut self, m: &mut Machine, args: &str) {
        let args = args.trim();
        if args.is_empty() {
            self.print(m, "Required parameter missing\r\n");
            return;
        }
        for file in args.split_whitespace() {
            let Some(mut data) = self.read_file(m, file) else {
                self.print(m, &format!("File not found - {file}\r\n"));
                return;
            };
            if let Some(end) = data.iter().position(|&b| b == 0x1A) {
                data.truncate(end);
            }
            self.write_handle(m, 1, &data);
        }
    }

    fn dir(&mut self, m: &mut Machine, args: &str) {
        let target = args.split_whitespace().find(|w| !w.starts_with('/')).unwrap_or("");
        let pattern = if target.is_empty() || target.ends_with('\\') || target.ends_with(':') {
            format!("{target}*.*")
        } else if self.is_dir(m, target) {
            format!("{target}\\*.*")
        } else {
            target.to_string()
        };
        let drive = char::from(b'A' + m.dos.drive);
        let cwd = m.dos.cwd[usize::from(m.dos.drive)].clone();
        let (dir, _) = split_dir(&pattern);
        let shown = if dir.is_empty() { format!("{drive}:\\{cwd}") } else { dir.trim_end_matches('\\').to_string() };
        self.print(m, &format!("\r\n Directory of {shown}\r\n\r\n"));
        let files = self.find(m, &pattern, 0x37);
        let (mut count, mut total) = (0, 0u64);
        for f in &files {
            let (base, extension) = f.name.split_once('.').unwrap_or((&f.name, ""));
            let size = if f.attr & 0x10 != 0 { "<DIR>     ".to_string() } else { format!("{:>10}", f.size) };
            let (day, month, year) = (f.date & 31, (f.date >> 5) & 15, (1980 + (f.date >> 9)) % 100);
            let (hour, minute) = (f.time >> 11, (f.time >> 5) & 63);
            let (h12, half) = (if hour % 12 == 0 { 12 } else { hour % 12 }, if hour < 12 { 'a' } else { 'p' });
            self.print(
                m,
                &format!(
                    "{base:<8} {extension:<3} {size} {month:02}-{day:02}-{year:02}  {h12:>2}:{minute:02}{half}\r\n"
                ),
            );
            if f.attr & 0x10 == 0 {
                count += 1;
                total += u64::from(f.size);
            }
        }
        if files.is_empty() {
            self.print(m, "File not found\r\n");
        } else {
            self.print(m, &format!("{count:>9} file(s) {total:>14} bytes\r\n"));
        }
    }

    // ---- redirection ----

    fn redirect(
        &mut self,
        m: &mut Machine,
        stdin: Option<&str>,
        stdout: Option<&(String, bool)>,
    ) -> Result<(), String> {
        let mut restore = Restore { stdin: None, stdout: None };
        if let Some(file) = stdin {
            let Some(handle) = self.open(m, file, 0x00) else { return Err("File not found\r\n".into()) };
            restore.stdin = self.replace_handle(m, 0, handle);
        }
        if let Some((file, append)) = stdout {
            let handle = if *append {
                self.open(m, file, 0x01).inspect(|&h| {
                    dos::call_service(m, callback::DOS, |r| {
                        r.set_ax(0x4202);
                        r.set_bx(h);
                        r.set_cx(0);
                        r.set_dx(0);
                    });
                })
            } else {
                None
            };
            let handle = handle.or_else(|| self.create(m, file));
            let Some(handle) = handle else {
                if let Some(saved) = restore.stdin {
                    self.restore_handle(m, 0, saved);
                }
                return Err("File creation error\r\n".into());
            };
            restore.stdout = self.replace_handle(m, 1, handle);
        }
        self.restore.push(restore);
        Ok(())
    }

    /// Undoes the redirections of the line that just ended.
    fn unredirect_one(&mut self, m: &mut Machine) {
        if let Some(restore) = self.restore.pop() {
            if let Some(saved) = restore.stdin {
                self.restore_handle(m, 0, saved);
            }
            if let Some(saved) = restore.stdout {
                self.restore_handle(m, 1, saved);
            }
        }
    }

    /// Undoes every redirection in force: the command that ran (inside a CALL or IF too) ended.
    fn unredirect(&mut self, m: &mut Machine) {
        while !self.restore.is_empty() {
            self.unredirect_one(m);
        }
    }

    /// Points standard handle `target` at `handle` (and closes that); returns a copy of the old one.
    fn replace_handle(&self, m: &mut Machine, target: u16, handle: u16) -> Option<u16> {
        let (r, failed) = dos::call_service(m, callback::DOS, |r| {
            r.set_ax(0x4500);
            r.set_bx(target);
        });
        let saved = (!failed).then(|| r.ax());
        dos::call_service(m, callback::DOS, |r| {
            r.set_ax(0x4600);
            r.set_bx(handle);
            r.set_cx(target);
        });
        self.close(m, handle);
        saved
    }

    fn restore_handle(&self, m: &mut Machine, target: u16, saved: u16) {
        dos::call_service(m, callback::DOS, |r| {
            r.set_ax(0x4600);
            r.set_bx(saved);
            r.set_cx(target);
        });
        self.close(m, saved);
    }

    // ---- DOS calls ----

    /// A DOS path function (AH) on one path at DS:DX; whether it succeeded.
    fn path_call(&self, m: &mut Machine, ah: u8, path: &str) -> bool {
        self.put_path(m, SCRATCH, path);
        let psp = self.psp;
        let (_, failed) = dos::call_service(m, callback::DOS, |r| {
            r.set_ax(u16::from(ah) << 8);
            r.seg[cpu::seg::DS] = psp;
            r.set_dx(SCRATCH);
        });
        !failed
    }

    /// Rename (AH=56h): DS:DX to ES:DI.
    fn two_path_call(&self, m: &mut Machine, ah: u8, from: &str, to: &str) -> bool {
        self.put_path(m, SCRATCH, from);
        self.put_path(m, SCRATCH2, to);
        let psp = self.psp;
        let (_, failed) = dos::call_service(m, callback::DOS, |r| {
            r.set_ax(u16::from(ah) << 8);
            r.seg[cpu::seg::DS] = psp;
            r.set_dx(SCRATCH);
            r.seg[cpu::seg::ES] = psp;
            r.set_di(SCRATCH2);
        });
        !failed
    }

    fn put_path(&self, m: &mut Machine, at: u16, path: &str) {
        let mut asciiz = bytes(path);
        asciiz.truncate(126);
        asciiz.push(0);
        m.mem.load(Machine::lin(self.psp, at), &asciiz);
    }

    fn open(&self, m: &mut Machine, path: &str, mode: u8) -> Option<u16> {
        self.put_path(m, SCRATCH, path);
        let psp = self.psp;
        let (r, failed) = dos::call_service(m, callback::DOS, |r| {
            r.set_ax(0x3D00 | u16::from(mode));
            r.seg[cpu::seg::DS] = psp;
            r.set_dx(SCRATCH);
        });
        (!failed).then(|| r.ax())
    }

    fn create(&self, m: &mut Machine, path: &str) -> Option<u16> {
        self.put_path(m, SCRATCH, path);
        let psp = self.psp;
        let (r, failed) = dos::call_service(m, callback::DOS, |r| {
            r.set_ax(0x3C00);
            r.set_cx(0);
            r.seg[cpu::seg::DS] = psp;
            r.set_dx(SCRATCH);
        });
        (!failed).then(|| r.ax())
    }

    fn close(&self, m: &mut Machine, handle: u16) {
        dos::call_service(m, callback::DOS, |r| {
            r.set_ax(0x3E00);
            r.set_bx(handle);
        });
    }

    fn read_file(&self, m: &mut Machine, path: &str) -> Option<Vec<u8>> {
        let handle = self.open(m, path, 0x00)?;
        let psp = self.psp;
        let mut data = Vec::new();
        loop {
            let (r, failed) = dos::call_service(m, callback::DOS, |r| {
                r.set_ax(0x3F00);
                r.set_bx(handle);
                r.set_cx(BUFFER_SIZE);
                r.seg[cpu::seg::DS] = psp;
                r.set_dx(BUFFER);
            });
            let n = if failed { 0 } else { r.ax() };
            data.extend((0..u32::from(n)).map(|i| m.mem.read_u8(Machine::lin(psp, BUFFER) + i)));
            if n < BUFFER_SIZE {
                break;
            }
        }
        self.close(m, handle);
        Some(data)
    }

    fn write_file(&self, m: &mut Machine, path: &str, data: &[u8]) -> bool {
        let Some(handle) = self.create(m, path) else { return false };
        let ok = self.write_handle(m, handle, data);
        self.close(m, handle);
        ok
    }

    fn write_handle(&self, m: &mut Machine, handle: u16, data: &[u8]) -> bool {
        let psp = self.psp;
        for chunk in data.chunks(usize::from(BUFFER_SIZE)) {
            m.mem.load(Machine::lin(psp, BUFFER), chunk);
            let (_, failed) = dos::call_service(m, callback::DOS, |r| {
                r.set_ax(0x4000);
                r.set_bx(handle);
                r.set_cx(chunk.len() as u16);
                r.seg[cpu::seg::DS] = psp;
                r.set_dx(BUFFER);
            });
            if failed {
                return false;
            }
        }
        true
    }

    /// Writes to standard output (handle 1, which may be redirected).
    fn print(&self, m: &mut Machine, text: &str) {
        self.write_handle(m, 1, &bytes(text));
    }

    fn is_file(&self, m: &mut Machine, path: &str) -> bool {
        self.attributes(m, path).is_some_and(|attr| attr & 0x18 == 0)
    }

    fn is_dir(&self, m: &mut Machine, path: &str) -> bool {
        self.attributes(m, path).is_some_and(|attr| attr & 0x10 != 0)
    }

    fn attributes(&self, m: &mut Machine, path: &str) -> Option<u16> {
        self.put_path(m, SCRATCH, path);
        let psp = self.psp;
        let (r, failed) = dos::call_service(m, callback::DOS, |r| {
            r.set_ax(0x4300);
            r.seg[cpu::seg::DS] = psp;
            r.set_dx(SCRATCH);
        });
        (!failed).then(|| r.cx())
    }

    /// Files matching `pattern` (FindFirst/FindNext with the shell's own DTA).
    fn find(&self, m: &mut Machine, pattern: &str, attr: u16) -> Vec<Found> {
        let saved = m.dos.dta;
        m.dos.dta = (self.psp, DTA);
        self.put_path(m, SCRATCH, pattern);
        let psp = self.psp;
        let mut found = Vec::new();
        let (_, mut failed) = dos::call_service(m, callback::DOS, |r| {
            r.set_ax(0x4E00);
            r.set_cx(attr);
            r.seg[cpu::seg::DS] = psp;
            r.set_dx(SCRATCH);
        });
        while !failed && found.len() < 4096 {
            let at = Machine::lin(psp, DTA);
            let name = text(&(0..13).map(|i| m.mem.read_u8(at + 0x1E + i)).take_while(|&b| b != 0).collect::<Vec<_>>());
            let attr = m.mem.read_u8(at + 0x15);
            if name != "." && name != ".." && attr & 0x08 == 0 {
                found.push(Found {
                    name,
                    attr,
                    time: m.mem.read_u16(at + 0x16),
                    date: m.mem.read_u16(at + 0x18),
                    size: m.mem.read_u32(at + 0x1A),
                });
            }
            failed = dos::call_service(m, callback::DOS, |r| r.set_ax(0x4F00)).1;
        }
        m.dos.dta = saved;
        found
    }

    // ---- environment ----

    fn variable(&self, m: &Machine, name: &str) -> Option<String> {
        environment(m, self.psp).into_iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v)
    }

    fn set_variable(&mut self, m: &mut Machine, name: &str, value: &str) {
        let name = name.to_ascii_uppercase();
        let mut strings = environment(m, self.psp);
        strings.retain(|(n, _)| *n != name);
        if !value.is_empty() {
            strings.push((name, value.to_string()));
        }
        let mut block: Vec<u8> = strings.iter().flat_map(|(n, v)| bytes(&format!("{n}={v}\0"))).collect();
        block.push(0);
        // After the strings: a count of 1 and the shell's path, as DOS 3+ has it.
        block.extend([1, 0]);
        block.extend(b"C:\\COMMAND.COM\0");
        let segment = m.mem.read_u16(Machine::lin(self.psp, 0x2C));
        // The block's size, from its memory control block.
        let capacity = usize::from(m.mem.read_u16(Machine::lin(segment.wrapping_sub(1), 3))) * 16;
        if segment == 0 || block.len() > capacity {
            self.print(m, "Out of environment space\r\n");
            return;
        }
        m.mem.load(Machine::lin(segment, 0), &block);
    }

    /// `%NAME%` and `%%` (in SET's value, as DOSBox does).
    fn expand_variables(&self, m: &Machine, value: &str) -> String {
        let mut out = String::new();
        let mut rest = value;
        while let Some(i) = rest.find('%') {
            out.push_str(&rest[..i]);
            rest = &rest[i + 1..];
            if let Some(after) = rest.strip_prefix('%') {
                out.push('%');
                rest = after;
            } else if let Some(end) = rest.find('%') {
                out.push_str(&self.variable(m, &rest[..end]).unwrap_or_default());
                rest = &rest[end + 1..];
            }
        }
        out.push_str(rest);
        out
    }

    /// A batch line's `%0`-`%9`, `%%` and `%NAME%` (DOSBox `ExpandedBatchLine`).
    fn expand(&self, m: &Machine, line: &str) -> String {
        let batch = self.batches.last().expect("in a batch file");
        let mut out = String::new();
        let mut rest = line;
        while let Some(i) = rest.find('%') {
            out.push_str(&rest[..i]);
            rest = &rest[i + 1..];
            let Some(c) = rest.chars().next() else { break };
            if c == '%' {
                out.push('%');
                rest = &rest[1..];
            } else if let Some(n) = c.to_digit(10) {
                if let Some(arg) = batch.args.get(batch.shift + n as usize) {
                    out.push_str(arg);
                }
                rest = &rest[1..];
            } else {
                let Some(end) = rest.find('%') else {
                    rest = "";
                    break;
                };
                out.push_str(&self.variable(m, &rest[..end]).unwrap_or_default());
                rest = &rest[end + 1..];
            }
        }
        out.push_str(rest);
        out
    }

    /// The prompt, from PROMPT's `$` codes.
    fn prompt(&self, m: &Machine) -> String {
        let spec = self.variable(m, "PROMPT").unwrap_or_else(|| "$N$G".into());
        let drive = char::from(b'A' + m.dos.drive);
        let mut out = String::new();
        let mut chars = spec.chars();
        while let Some(c) = chars.next() {
            if c != '$' {
                out.push(c);
                continue;
            }
            match chars.next().map(|c| c.to_ascii_uppercase()) {
                Some('P') => out.push_str(&format!("{drive}:\\{}", m.dos.cwd[usize::from(m.dos.drive)])),
                Some('N') => out.push(drive),
                Some('G') => out.push('>'),
                Some('L') => out.push('<'),
                Some('B') => out.push('|'),
                Some('Q') => out.push('='),
                Some('$') => out.push('$'),
                Some('_') => out.push_str("\r\n"),
                Some('E') => out.push('\u{1B}'),
                Some('H') => out.push('\u{8}'),
                Some('V') => out.push_str("MS-DOS Version 5.00"),
                _ => {}
            }
        }
        out
    }
}

struct Found {
    name: String,
    attr: u8,
    time: u16,
    date: u16,
    size: u32,
}

impl Batch {
    /// The next line that isn't empty or a label (DOSBox `ReadLine`), control characters other
    /// than tab, backspace and escape dropped; None at the end (or a Ctrl-Z).
    fn read_line(&mut self) -> Option<String> {
        loop {
            let line = self.raw_line()?;
            let first = line.find(|c| !matches!(c, '=' | '\t' | ' '));
            if line.is_empty() || first.is_some_and(|i| line[i..].starts_with(':')) {
                continue;
            }
            return Some(line);
        }
    }

    fn raw_line(&mut self) -> Option<String> {
        if self.pos >= self.text.len() || self.text[self.pos] == 0x1A {
            return None;
        }
        let rest = &self.text[self.pos..];
        let end = rest.iter().position(|&b| b == b'\n' || b == 0x1A).unwrap_or(rest.len());
        self.pos += end + usize::from(rest.get(end) == Some(&b'\n'));
        let line: Vec<u8> = rest[..end].iter().copied().filter(|&b| b > 31 || matches!(b, b'\t' | 8 | 27)).collect();
        Some(text(&line).trim().to_string())
    }

    /// From the start: the line after `:label`.
    fn goto(&mut self, label: &str) -> bool {
        self.pos = 0;
        while let Some(line) = self.raw_line() {
            let start = line.find(|c| !matches!(c, '=' | '\t' | ' ' | ':'));
            let Some(start) = start else { continue };
            if line[..start].matches(':').count() != 1 {
                continue;
            }
            let name = line[start..].split(['\t', ' ']).next().unwrap_or("");
            if name.eq_ignore_ascii_case(label) {
                return true;
            }
        }
        false
    }
}

/// Characters of `line` outside double quotes, with their byte positions.
fn outside_quotes(line: &str) -> impl Iterator<Item = (usize, char)> + '_ {
    let mut quoted = false;
    line.char_indices().filter(move |&(_, c)| {
        if c == '"' {
            quoted = !quoted;
        }
        !quoted && c != '"'
    })
}

struct Redirections {
    line: String,
    stdin: Option<String>,
    /// The file, and whether to append (>>).
    stdout: Option<(String, bool)>,
}

/// Takes `<file`, `>file` and `>>file` out of a line (outside quotes); a target may end with a
/// colon (`>NUL:`). None: a redirection without a target.
fn redirections(line: &str) -> Option<Redirections> {
    let mut out = Redirections { line: String::new(), stdin: None, stdout: None };
    let chars: Vec<char> = line.chars().collect();
    let (mut i, mut quoted) = (0, false);
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            quoted = !quoted;
        }
        if quoted || !matches!(c, '<' | '>') {
            out.line.push(c);
            i += 1;
            continue;
        }
        let append = c == '>' && chars.get(i + 1) == Some(&'>');
        i += if append { 2 } else { 1 };
        while i < chars.len() && matches!(chars[i], ' ' | '\t') {
            i += 1;
        }
        let start = i;
        while i < chars.len() && !matches!(chars[i], '<' | '>' | '|' | ' ' | '\t') {
            i += 1;
        }
        let mut target: String = chars[start..i].iter().collect();
        if target.ends_with(':') && target.len() > 2 {
            target.pop();
        }
        if target.is_empty() {
            return None;
        }
        if c == '<' {
            out.stdin = Some(target);
        } else {
            out.stdout = Some((target, append));
        }
    }
    Some(out)
}

fn is_internal(name: &str) -> bool {
    const NAMES: [&str; 34] = [
        "REM", "ECHO", "CLS", "PAUSE", "SET", "PATH", "PROMPT", "CD", "CHDIR", "MD", "MKDIR", "RD", "RMDIR", "DEL",
        "ERASE", "REN", "RENAME", "COPY", "TYPE", "DIR", "VER", "EXIT", "IF", "GOTO", "SHIFT", "CALL", "FOR", "CHOICE",
        "LH", "LOADHIGH", "CTTY", "BREAK", "VERIFY", "TRUENAME",
    ];
    NAMES.iter().any(|n| n.eq_ignore_ascii_case(name))
}

/// Whether `text` starts with `word`, any case.
fn starts_with_word(text: &str, word: &str) -> bool {
    text.get(..word.len()).is_some_and(|start| start.eq_ignore_ascii_case(word))
}

/// The first word (up to a space or tab) and the rest.
fn split_word(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    let end = text.find([' ', '\t']).unwrap_or(text.len());
    (&text[..end], &text[end..])
}

/// A path's folder part (with its backslash or drive colon) and its name.
fn split_dir(path: &str) -> (&str, &str) {
    match path.rfind(['\\', ':']) {
        Some(i) => path.split_at(i + 1),
        None => ("", path),
    }
}

fn folder(path: &str) -> String {
    if path.is_empty() || path.ends_with('\\') || path.ends_with(':') { path.to_string() } else { format!("{path}\\") }
}

/// REN's pattern: `?` keeps the name's character there, `*` the rest of the part.
fn rename_pattern(name: &str, pattern: &str) -> String {
    let (base, extension) = name.split_once('.').unwrap_or((name, ""));
    let (pattern_base, pattern_extension) = pattern.split_once('.').unwrap_or((pattern, ""));
    let part = |from: &str, pattern: &str| {
        let from: Vec<char> = from.chars().collect();
        let mut out = String::new();
        for (i, c) in pattern.chars().enumerate() {
            match c {
                '*' => {
                    out.extend(from.iter().skip(i));
                    break;
                }
                '?' => out.extend(from.get(i)),
                c => out.push(c),
            }
        }
        out
    };
    let base = part(base, pattern_base);
    let extension = part(extension, pattern_extension);
    if extension.is_empty() { base } else { format!("{base}.{extension}") }
}

/// The process's environment strings, as (name, value).
fn environment(m: &Machine, psp: u16) -> Vec<(String, String)> {
    let segment = m.mem.read_u16(Machine::lin(psp, 0x2C));
    let mut out = Vec::new();
    let mut at = Machine::lin(segment, 0);
    for _ in 0..256 {
        let entry: Vec<u8> =
            (0..u32::from(ENVIRONMENT_BYTES)).map(|i| m.mem.read_u8(at + i)).take_while(|&b| b != 0).collect();
        if entry.is_empty() {
            break;
        }
        at += entry.len() as u32 + 1;
        let entry = text(&entry);
        if let Some((name, value)) = entry.split_once('=') {
            out.push((name.to_string(), value.to_string()));
        }
    }
    out
}

/// A larger environment for the shell, so that SET has room: its strings copied into 1 KB.
fn give_environment_room(m: &mut Machine, psp: u16) {
    let old = m.mem.read_u16(Machine::lin(psp, 0x2C));
    let strings = if old == 0 { Vec::new() } else { environment(m, psp) };
    let Ok(segment) = dos::allocate(m, ENVIRONMENT_BYTES / 16, psp) else { return };
    let mut block: Vec<u8> = strings.iter().flat_map(|(n, v)| bytes(&format!("{n}={v}\0"))).collect();
    block.push(0);
    block.extend([1, 0]);
    block.extend(b"C:\\COMMAND.COM\0");
    block.truncate(usize::from(ENVIRONMENT_BYTES));
    m.mem.load(Machine::lin(segment, 0), &block);
    m.mem.write_u16(Machine::lin(psp, 0x2C), segment);
    if old != 0 {
        let _ = dos::free(m, old);
    }
}
