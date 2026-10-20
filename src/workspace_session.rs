//! Ephemeral, explicitly inherited context for external workspace runners.
//!
//! The parent owns routes and port reservations. Members claim a declared cwd,
//! wait for the parent to release its reservation, and publish process identity
//! synchronously with the durable registry before reporting a successful spawn.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::cli::RunArgs;
use crate::config::ResolvedRunConfig;
use crate::framework::FrameworkChoice;
use crate::process::{self, Liveness, ManagedChild};
use crate::state::{Lease, LeaseState, Scheme, Store, WorkspaceSupervisor};

pub(crate) const SESSION_VARIABLE: &str = "NOOK_WORKSPACE_SESSION";
const VERSION: u32 = 1;
const MAX_CONTEXT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionApp {
    pub(crate) id: String,
    #[serde(with = "os_path")]
    pub(crate) directory: PathBuf,
    pub(crate) lease_id: Uuid,
    pub(crate) hostname: String,
    pub(crate) port: u16,
    pub(crate) bind_address: IpAddr,
    pub(crate) tls: bool,
    pub(crate) strict_port: bool,
    pub(crate) readiness_warn_after_seconds: u64,
    /// `auto`, `none`, `vite`, `nuxt`, or `astro`.
    pub(crate) framework: String,
    pub(crate) environment: Vec<(OsString, OsString)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionMembership {
    pub(crate) app_id: String,
    pub(crate) token: Uuid,
    pub(crate) wrapper: Lease,
    pub(crate) child: Option<Lease>,
    pub(crate) released: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Context {
    version: u32,
    closing: bool,
    #[serde(with = "os_path")]
    root: PathBuf,
    group_id: Uuid,
    #[serde(with = "os_path")]
    state_path: PathBuf,
    supervisor: Lease,
    apps: Vec<SessionApp>,
    members: Vec<SessionMembership>,
}

// Path's serde implementation rejects non-Unicode paths. OsString preserves
// native Unix bytes and Windows code units, as it does for member environment.
mod os_path {
    use super::*;

    pub(super) fn serialize<S: serde::Serializer>(
        path: &Path,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        path.as_os_str().serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<PathBuf, D::Error> {
        OsString::deserialize(deserializer).map(PathBuf::from)
    }
}

pub(crate) struct SessionOwner {
    path: PathBuf,
    directory: PathBuf,
}

pub(crate) struct SessionMember {
    path: PathBuf,
    app: SessionApp,
    token: Uuid,
    state_path: PathBuf,
    supervisor: Lease,
    group_id: Uuid,
    finished: bool,
}

fn invalid(message: impl Into<String>) -> crate::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("workspace session: {}", message.into()),
    )
    .into()
}

fn identity_lease(pid: u32, id: Uuid, group_id: Uuid) -> crate::Result<Lease> {
    let identity = process::read_process_identity(pid)
        .ok_or_else(|| invalid(format!("cannot verify process identity for PID {pid}")))?;
    Ok(Lease {
        id,
        group_id: Some(group_id),
        workspace_supervisor: None,
        lan: false,
        hostname: String::new(),
        target: String::new(),
        scheme: Scheme::Http,
        tls: false,
        pid,
        pgid: identity.pgid,
        process_start_time_ticks: identity.start_time_ticks,
        state: LeaseState::Starting,
    })
}

fn require_live(context: &Context) -> crate::Result<()> {
    if context.version != VERSION {
        return Err(invalid("unsupported context version"));
    }
    if context.closing {
        return Err(invalid("the owning Nook process is shutting down"));
    }
    if process::workspace_supervisor_liveness(&WorkspaceSupervisor {
        pid: context.supervisor.pid,
        process_start_time_ticks: context.supervisor.process_start_time_ticks,
    }) != Liveness::Alive
    {
        return Err(invalid(
            "the owning Nook process has exited or its identity cannot be verified",
        ));
    }
    Ok(())
}

/// A stable lock synchronizes atomic context replacements across processes.
/// Crash-left temporary snapshots are removed with the private session directory.
struct LockedContext {
    _lock: File,
    path: PathBuf,
    context: Context,
}

fn open_private_file(path: &Path, create_new: bool) -> crate::Result<File> {
    if !create_new {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| invalid(format!("cannot open supplied context: {error}")))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(invalid("context must be a regular file, not a symlink"));
        }
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(create_new);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let opened = file.metadata()?;
        let directory = fs::symlink_metadata(path.parent().expect("absolute file path"))?;
        let current_uid = fs::metadata("/proc/self")?.uid();
        if opened.uid() != current_uid
            || directory.uid() != current_uid
            || opened.mode() & 0o077 != 0
            || directory.mode() & 0o077 != 0
            || directory.file_type().is_symlink()
        {
            return Err(invalid(
                "context must be owned by this user with private file and directory permissions",
            ));
        }
    }
    Ok(file)
}

impl LockedContext {
    fn open(path: &Path) -> crate::Result<Self> {
        if !path.is_absolute() || path.file_name() != Some("context.json".as_ref()) {
            return Err(invalid(
                "marker must name an absolute session context.json path",
            ));
        }
        // Open rather than create: invalid or removed contexts never resurrect.
        let lock = open_private_file(&path.with_file_name("context.lock"), false)?;
        lock.lock()?;
        let file = open_private_file(path, false)?;
        if file.metadata()?.len() > MAX_CONTEXT_BYTES {
            return Err(invalid("context is too large"));
        }
        let context = serde_json::from_reader((&file).take(MAX_CONTEXT_BYTES))
            .map_err(|error| invalid(format!("invalid context: {error}")))?;
        Ok(Self {
            _lock: lock,
            path: path.to_owned(),
            context,
        })
    }

    fn save(&mut self) -> crate::Result<()> {
        let contents = serde_json::to_vec(&self.context)
            .map_err(|error| invalid(format!("cannot encode context: {error}")))?;
        if contents.len() as u64 > MAX_CONTEXT_BYTES {
            return Err(invalid("context is too large"));
        }
        let temporary = self
            .path
            .with_file_name(format!("context-{}.tmp", Uuid::new_v4()));
        let result = (|| -> crate::Result<()> {
            let mut file = open_private_file(&temporary, true)?;
            file.write_all(&contents)?;
            file.sync_all()?;
            drop(file); // Required before replacement on Windows.
            crate::platform::replace_file(&temporary, &self.path)?;
            crate::platform::sync_directory(self.path.parent().expect("absolute file path"))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

impl SessionOwner {
    pub(crate) fn create(
        root: PathBuf,
        group_id: Uuid,
        state_path: PathBuf,
        mut apps: Vec<SessionApp>,
    ) -> crate::Result<Self> {
        let root = fs::canonicalize(root)?;
        if !state_path.is_absolute() {
            return Err(invalid("state store path must be absolute"));
        }
        for index in 0..apps.len() {
            apps[index].directory = fs::canonicalize(&apps[index].directory)?;
            let app = &apps[index];
            if !app.directory.is_dir() || !app.directory.starts_with(&root) || app.port == 0 {
                return Err(invalid(format!(
                    "invalid directory or port for app `{}`",
                    app.id
                )));
            }
            framework_choice(&app.framework)?;
            if apps[..index].iter().any(|other| {
                other.id == app.id
                    || other.directory == app.directory
                    || other.lease_id == app.lease_id
            }) {
                return Err(invalid(
                    "app IDs, directories, and lease owners must be unique",
                ));
            }
        }
        let directory = session_directory(group_id);
        create_private_directory(&directory)?;
        let owner = Self {
            path: directory.join("context.json"),
            directory,
        };
        let context = Context {
            version: VERSION,
            closing: false,
            root,
            group_id,
            state_path,
            supervisor: identity_lease(std::process::id(), group_id, group_id)?,
            apps,
            members: Vec::new(),
        };
        let lock = open_private_file(&owner.path.with_file_name("context.lock"), true)?;
        lock.lock()?;
        LockedContext {
            _lock: lock,
            path: owner.path.clone(),
            context,
        }
        .save()?;
        Ok(owner)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The callback drops precisely this app's reservation. It runs under the
    /// session lock before the member is allowed to bind its assigned port.
    pub(crate) fn poll_joins(
        &self,
        mut release_port: impl FnMut(&SessionApp, &Lease) -> crate::Result<()>,
    ) -> crate::Result<()> {
        let mut locked = LockedContext::open(&self.path)?;
        require_live(&locked.context)?;
        let mut changed = false;
        for index in 0..locked.context.members.len() {
            let member = &locked.context.members[index];
            if member.released {
                continue;
            }
            if process::lease_liveness(&member.wrapper) != Liveness::Alive {
                continue;
            }
            let app = locked
                .context
                .apps
                .iter()
                .find(|app| app.id == member.app_id)
                .ok_or_else(|| invalid("claim references an undeclared application"))?;
            release_port(app, &member.wrapper)?;
            locked.context.members[index].released = true;
            changed = true;
        }
        if changed {
            locked.save()?;
        }
        Ok(())
    }

    pub(crate) fn members(&self) -> crate::Result<Vec<SessionMembership>> {
        Ok(LockedContext::open(&self.path)?.context.members)
    }

    /// Freeze claims and registrations before the shutdown process snapshot.
    /// The context remains readable until the parent completes tree cleanup.
    pub(crate) fn close(&self) -> crate::Result<Vec<SessionMembership>> {
        let mut locked = LockedContext::open(&self.path)?;
        locked.context.closing = true;
        locked.save()?;
        Ok(locked.context.members.clone())
    }
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::mem::{size_of, zeroed};
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{addr_of_mut, null_mut};
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAceEx, GetLengthSid,
        GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor, SE_DACL_PROTECTED,
        SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SetSecurityDescriptorControl,
        SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{CreateDirectoryW, FILE_ALL_ACCESS};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        let mut size = 0;
        unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut size) };
        // usize storage gives TOKEN_USER its required pointer alignment.
        let mut user = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
        if unsafe {
            GetTokenInformation(token, TokenUser, user.as_mut_ptr().cast(), size, &mut size)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let sid = unsafe { (*(user.as_ptr().cast::<TOKEN_USER>())).User.Sid };
        let sid_size = unsafe { GetLengthSid(sid) } as usize;
        let acl_size =
            size_of::<ACL>() + size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>() + sid_size;
        let mut acl = vec![0u32; acl_size.div_ceil(size_of::<u32>())];
        let acl = acl.as_mut_ptr().cast::<ACL>();
        // OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE: context files inherit the
        // single current-user grant rather than a permissive temp-folder ACL.
        if unsafe { InitializeAcl(acl, acl_size as u32, ACL_REVISION) } == 0
            || unsafe { AddAccessAllowedAceEx(acl, ACL_REVISION, 3, FILE_ALL_ACCESS, sid) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut descriptor: SECURITY_DESCRIPTOR = unsafe { zeroed() };
        let descriptor_pointer = addr_of_mut!(descriptor).cast();
        if unsafe { InitializeSecurityDescriptor(descriptor_pointer, 1) } == 0
            || unsafe { SetSecurityDescriptorDacl(descriptor_pointer, 1, acl, 0) } == 0
            || unsafe {
                SetSecurityDescriptorControl(
                    descriptor_pointer,
                    SE_DACL_PROTECTED,
                    SE_DACL_PROTECTED,
                )
            } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor_pointer,
            bInheritHandle: 0,
        };
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe { CreateDirectoryW(path.as_ptr(), &attributes) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    })();
    unsafe { CloseHandle(token) };
    result
}

impl Drop for SessionOwner {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn framework_choice(value: &str) -> crate::Result<FrameworkChoice> {
    if value == "auto" {
        Ok(FrameworkChoice::Auto)
    } else {
        FrameworkChoice::parse(value).map_err(invalid)
    }
}

fn session_directory(group_id: Uuid) -> PathBuf {
    std::env::temp_dir().join(format!("nook-session-{group_id}"))
}

/// Remove crash-left environment only after the caller has cleaned every
/// process in this workspace group. Never erase a live, unverifiable, or
/// mismatched context, even if a stale registry entry names its group UUID.
pub(crate) fn cleanup_stale(group_id: Uuid, supervisor: &WorkspaceSupervisor) -> crate::Result<()> {
    let directory = session_directory(group_id);
    let path = directory.join("context.json");
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let locked = LockedContext::open(&path)?;
    if locked.context.group_id != group_id
        || locked.context.supervisor.pid != supervisor.pid
        || locked.context.supervisor.process_start_time_ticks != supervisor.process_start_time_ticks
    {
        return Err(invalid(
            "refusing to clean a context with mismatched group or supervisor identity",
        ));
    }
    if process::workspace_supervisor_liveness(supervisor) != Liveness::Dead {
        return Err(invalid(
            "refusing to clean a live or unverifiable supervisor context",
        ));
    }
    drop(locked); // Windows cannot unlink the context while its lock is open.
    match fs::remove_dir_all(&directory) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        result => result?,
    }
    Ok(())
}

impl SessionMember {
    /// A present marker always takes this path; callers must propagate errors
    /// instead of falling through to standalone or recursively loading a root.
    pub(crate) fn claim(path: &Path, cwd: &Path, args: &RunArgs) -> crate::Result<Self> {
        let directory = fs::canonicalize(cwd)?;
        let mut locked = LockedContext::open(path)?;
        let context = &mut locked.context;
        require_live(context)?;
        let matching: Vec<_> = context
            .apps
            .iter()
            .filter(|app| app.directory == directory)
            .collect();
        let [app] = matching.as_slice() else {
            return Err(invalid(format!(
                "working directory `{}` must match exactly one declared app directory",
                directory.display()
            )));
        };
        let app = (*app).clone();
        validate_arguments(&app, args)?;
        if let Some(member) = context
            .members
            .iter()
            .find(|member| member.app_id == app.id)
        {
            // Unknown identity is never a license to steal a claim. A surviving
            // independent child also keeps the claim after its wrapper exits.
            if process::lease_liveness(&member.wrapper) != Liveness::Dead
                || member
                    .child
                    .as_ref()
                    .is_some_and(|child| process::lease_liveness(child) != Liveness::Dead)
            {
                return Err(invalid(format!(
                    "app `{}` is already claimed by a live or unverifiable member",
                    app.id
                )));
            }
        }
        let token = Uuid::new_v4();
        let group_id = context.group_id;
        let state_path = context.state_path.clone();
        let supervisor = context.supervisor.clone();
        let wrapper = identity_lease(std::process::id(), app.lease_id, group_id)?;
        context.members.retain(|claim| claim.app_id != app.id);
        context.members.push(SessionMembership {
            app_id: app.id.clone(),
            token,
            wrapper,
            child: None,
            released: false,
        });
        locked.save()?;
        Ok(Self {
            path: path.to_owned(),
            app,
            token,
            state_path,
            supervisor,
            group_id,
            finished: false,
        })
    }

    pub(crate) fn app(&self) -> &SessionApp {
        &self.app
    }
    pub(crate) fn state_path(&self) -> &Path {
        &self.state_path
    }
    pub(crate) fn supervisor(&self) -> &Lease {
        &self.supervisor
    }
    pub(crate) fn group_id(&self) -> Uuid {
        self.group_id
    }

    pub(crate) fn config(&self, args: &RunArgs) -> crate::Result<ResolvedRunConfig> {
        validate_arguments(&self.app, args)?;
        let framework = if args.no_framework {
            FrameworkChoice::Disabled
        } else if let Some(value) = &args.framework {
            framework_choice(value)?
        } else {
            framework_choice(&self.app.framework)?
        };
        Ok(ResolvedRunConfig {
            hostname: self.app.hostname.clone(),
            command: args.command.clone(),
            tls: self.app.tls,
            tailscale: false,
            funnel: false,
            lan: false,
            app_port: Some(self.app.port),
            strict_port: self.app.strict_port,
            force: false,
            readiness_warn_after_seconds: args
                .readiness_warn_after
                .unwrap_or(self.app.readiness_warn_after_seconds),
            bind_address: self.app.bind_address,
            ignored_local_config: None,
            framework,
        })
    }

    pub(crate) fn wait_for_release(&self) -> crate::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut signals = process::ForwardedSignals::new()?;
        loop {
            let locked = LockedContext::open(&self.path)?;
            require_live(&locked.context)?;
            let claim = self.claim_in(&locked.context)?;
            if claim.released {
                return Ok(());
            }
            if !signals.pending().is_empty() {
                return Err(invalid("interrupted while waiting to join"));
            }
            if Instant::now() >= deadline {
                return Err(invalid(
                    "owner did not release the app port within 30 seconds",
                ));
            }
            drop(locked);
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// A cheap member supervision tick without installing signal handlers.
    pub(crate) fn check_live(&self) -> crate::Result<()> {
        let locked = LockedContext::open(&self.path)?;
        require_live(&locked.context)?;
        self.claim_in(&locked.context)?;
        Ok(())
    }

    /// Synchronize launch and publication with the owner's closing snapshot.
    /// The spawn closure must use the existing managed-child process primitive.
    pub(crate) fn spawn_child(
        &self,
        spawn: impl FnOnce() -> crate::Result<ManagedChild>,
        associate: impl FnOnce(&Store, &SessionApp, &Lease, &Lease) -> crate::Result<()>,
    ) -> crate::Result<ManagedChild> {
        let mut locked = LockedContext::open(&self.path)?;
        self.require_unstarted(&locked.context)?;
        let mut child = spawn()?;
        if let Err(error) = self.register_locked(&mut locked, &child, associate) {
            let _ = child.signal(process::ProcessSignal::Kill);
            let _ = child.wait();
            child.join_output();
            return Err(error);
        }
        Ok(child)
    }

    /// The callback MUST durably replace this app's placeholder with `child`,
    /// including its supervisor identity, before returning. Parent polling is
    /// deliberately not the durability boundary for a newly spawned process.
    #[cfg(test)]
    pub(crate) fn register_child(
        &self,
        child: &ManagedChild,
        associate: impl FnOnce(&Store, &SessionApp, &Lease, &Lease) -> crate::Result<()>,
    ) -> crate::Result<()> {
        let mut locked = LockedContext::open(&self.path)?;
        self.register_locked(&mut locked, child, associate)
    }

    fn require_unstarted(&self, context: &Context) -> crate::Result<()> {
        require_live(context)?;
        let claim = self.claim_in(context)?;
        if !claim.released || claim.child.is_some() {
            return Err(invalid(
                "child registration requires one acknowledged, unstarted claim",
            ));
        }
        Ok(())
    }

    fn register_locked(
        &self,
        locked: &mut LockedContext,
        child: &ManagedChild,
        associate: impl FnOnce(&Store, &SessionApp, &Lease, &Lease) -> crate::Result<()>,
    ) -> crate::Result<()> {
        self.require_unstarted(&locked.context)?;
        let mut lease = identity_lease(child.pid, self.app.lease_id, self.group_id)?;
        if lease.pgid != child.pgid || lease.process_start_time_ticks != child.start_time_ticks {
            return Err(invalid(
                "spawned child identity changed before registration",
            ));
        }
        lease.hostname = self.app.hostname.clone();
        lease.target = format!(
            "http://{}",
            std::net::SocketAddr::new(
                process::readiness_probe_address(self.app.bind_address),
                self.app.port
            )
        );
        lease.tls = self.app.tls;
        lease.workspace_supervisor = Some(WorkspaceSupervisor {
            pid: self.supervisor.pid,
            process_start_time_ticks: self.supervisor.process_start_time_ticks,
        });
        associate(
            &Store::new(self.state_path.clone()),
            &self.app,
            &self.supervisor,
            &lease,
        )?;
        // Keep the same durable identity in context for parent shutdown cleanup.
        locked
            .context
            .members
            .iter_mut()
            .find(|claim| claim.token == self.token)
            .expect("claim verified under lock")
            .child = Some(lease);
        locked.save()
    }

    fn claim_in<'a>(&self, context: &'a Context) -> crate::Result<&'a SessionMembership> {
        if context.group_id != self.group_id
            || context.supervisor != self.supervisor
            || context.state_path != self.state_path
        {
            return Err(invalid("owning session identity changed"));
        }
        context
            .members
            .iter()
            .find(|claim| claim.token == self.token && claim.app_id == self.app.id)
            .ok_or_else(|| invalid("member claim was withdrawn or replaced"))
    }

    /// Call only after stopping and reaping the owned child group. Failure to
    /// clear a claim is conservative: a live child must not be joined twice.
    pub(crate) fn finish(&mut self) -> crate::Result<()> {
        let mut locked = LockedContext::open(&self.path)?;
        self.claim_in(&locked.context)?;
        locked
            .context
            .members
            .retain(|claim| claim.token != self.token);
        locked.save()?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for SessionMember {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        // A failed startup can drop an unstarted claim. A registered process
        // remains recorded until explicit cleanup or owner teardown.
        if let Ok(mut locked) = LockedContext::open(&self.path)
            && locked
                .context
                .members
                .iter()
                .any(|claim| claim.token == self.token && claim.child.is_none())
        {
            locked
                .context
                .members
                .retain(|claim| claim.token != self.token);
            let _ = locked.save();
        }
    }
}

fn validate_arguments(app: &SessionApp, args: &RunArgs) -> crate::Result<()> {
    // Existing --name in package scripts is accepted; canonical cwd is the
    // sole app selector and the session's hostname remains authoritative.
    if args.command.is_empty() {
        return Err(invalid("a member command is required after --"));
    }
    if args.runner || args.only.is_some() || args.config.is_some() || args.local || args.force {
        return Err(invalid(
            "members cannot use --runner, --only, --config, --local, or --force; configure the root session",
        ));
    }
    if args.tailscale || args.funnel || args.lan {
        return Err(invalid(
            "member exposure overrides are incompatible with a workspace session",
        ));
    }
    if args.app_port.is_some_and(|port| port != app.port)
        || args
            .run_bind_address
            .is_some_and(|address| address != app.bind_address)
        || (args.no_tls && app.tls)
    {
        return Err(invalid(
            "member port, bind address, and TLS must match the root session; remove the incompatible override",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    struct Fixture {
        // Drop the session before deleting the app directories.
        owner: SessionOwner,
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("nook-session-test-{}", Uuid::new_v4()));
            fs::create_dir_all(root.join("app/nested")).unwrap();
            fs::create_dir_all(root.join("other")).unwrap();
            let root = fs::canonicalize(root).unwrap();
            let app = SessionApp {
                id: "web".into(),
                directory: root.join("app"),
                lease_id: Uuid::new_v4(),
                hostname: "web.demo.localhost".into(),
                port: 43210,
                bind_address: "127.0.0.1".parse().unwrap(),
                tls: true,
                strict_port: false,
                readiness_warn_after_seconds: 30,
                framework: "auto".into(),
                environment: vec![("BACKEND_URL".into(), "http://127.0.0.1:43211".into())],
            };
            let owner = SessionOwner::create(
                root.clone(),
                Uuid::new_v4(),
                root.join("state.json"),
                vec![app],
            )
            .unwrap();
            Self { owner, root }
        }

        fn claim(&self, flags: &[&str]) -> crate::Result<SessionMember> {
            SessionMember::claim(self.owner.path(), &self.root.join("app"), &args(flags))
        }

        fn change(&self, update: impl FnOnce(&mut Context)) {
            let mut locked = LockedContext::open(self.owner.path()).unwrap();
            update(&mut locked.context);
            locked.save().unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn args(flags: &[&str]) -> RunArgs {
        let cli = crate::cli::Cli::try_parse_from(
            ["nook", "run"]
                .into_iter()
                .chain(flags.iter().copied())
                .chain(["--", "nuxt", "dev", "--port", "3000"]),
        )
        .unwrap();
        let crate::cli::Command::Run(args) = cli.command else {
            unreachable!()
        };
        args
    }

    fn error<T>(result: crate::Result<T>, expected: &str) {
        let error = result.err().expect("expected a session error").to_string();
        assert!(
            error.contains(expected),
            "expected {expected:?}, got {error:?}"
        );
    }

    #[test]
    fn valid_claim_releases_only_after_owner_ack_and_preserves_package_argv() {
        let fixture = Fixture::new();
        let mut member = fixture.claim(&["--name", "old-standalone-name"]).unwrap();
        let claims = fixture.owner.members().unwrap();
        assert_eq!(claims.len(), 1);
        assert!(!claims[0].released);
        assert_eq!(member.state_path(), fixture.root.join("state.json"));
        assert_eq!(member.supervisor().pid, std::process::id());
        assert_eq!(member.group_id(), claims[0].wrapper.group_id.unwrap());
        let mut releases = 0;
        fixture
            .owner
            .poll_joins(|app, wrapper| {
                assert_eq!(app.id, "web");
                assert_eq!(wrapper.pid, std::process::id());
                releases += 1;
                Ok(())
            })
            .unwrap();
        fixture
            .owner
            .poll_joins(|_, _| panic!("an acknowledged reservation must not release twice"))
            .unwrap();
        assert_eq!(releases, 1);
        member.wait_for_release().unwrap();
        let config = member.config(&args(&[])).unwrap();
        assert_eq!(config.command, ["nuxt", "dev", "--port", "3000"]);
        assert_eq!(config.hostname, "web.demo.localhost");
        assert_eq!(config.app_port, Some(43210));
        assert_eq!(config.framework, FrameworkChoice::Auto);
        assert_eq!(member.app().environment[0].1, "http://127.0.0.1:43211");
        member.finish().unwrap();
        assert!(fixture.owner.members().unwrap().is_empty());
        fixture.claim(&[]).unwrap().finish().unwrap();
    }

    #[test]
    fn cwd_is_an_exact_canonical_directory_and_claims_are_exclusive() {
        let fixture = Fixture::new();
        for cwd in [
            &fixture.root,
            &fixture.root.join("other"),
            &fixture.root.join("app/nested"),
        ] {
            error(
                SessionMember::claim(fixture.owner.path(), cwd, &args(&[])),
                "match exactly one",
            );
        }
        let mut member = fixture.claim(&[]).unwrap();
        error(fixture.claim(&[]), "already claimed");
        member.finish().unwrap();
        let alias = fixture.root.join("app/nested/..");
        SessionMember::claim(fixture.owner.path(), &alias, &args(&[]))
            .unwrap()
            .finish()
            .unwrap();
    }

    #[test]
    fn incompatible_overrides_fail_before_creating_a_claim() {
        let fixture = Fixture::new();
        for flags in [
            vec!["--app-port", "3000"],
            vec!["--run-bind-address", "0.0.0.0"],
            vec!["--no-tls"],
            vec!["--tailscale"],
            vec!["--funnel"],
            vec!["--lan"],
            vec!["--force"],
            vec!["--only", "web"],
            vec!["--config", "member.toml"],
        ] {
            assert!(fixture.claim(&flags).is_err(), "{flags:?} must fail");
            assert!(fixture.owner.members().unwrap().is_empty());
        }
        fixture
            .claim(&[
                "--app-port",
                "43210",
                "--strict-port",
                "--run-bind-address",
                "127.0.0.1",
                "--no-lan",
                "--no-tailscale",
            ])
            .unwrap()
            .finish()
            .unwrap();
        let mut member = fixture.claim(&["--framework", "nuxt"]).unwrap();
        assert_eq!(
            member
                .config(&args(&["--framework", "nuxt"]))
                .unwrap()
                .framework,
            FrameworkChoice::parse("nuxt").unwrap()
        );
        member.finish().unwrap();
    }

    #[test]
    fn invalid_context_and_wrong_start_identity_fail_closed() {
        let fixture = Fixture::new();
        error(
            SessionMember::claim(Path::new(""), &fixture.root, &args(&[])),
            "absolute session",
        );
        error(
            SessionMember::claim(
                &fixture.root.join("context.json"),
                &fixture.root,
                &args(&[]),
            ),
            "cannot open supplied",
        );
        fixture.change(|context| context.supervisor.process_start_time_ticks += 1);
        error(fixture.claim(&[]), "identity cannot be verified");
        fixture.change(|context| {
            context.supervisor.process_start_time_ticks -= 1;
            context.version += 1;
        });
        error(fixture.claim(&[]), "unsupported context version");
        fs::write(fixture.owner.path(), "{invalid json").unwrap();
        error(fixture.claim(&[]), "invalid context");
    }

    #[test]
    fn closing_is_atomic_and_blocks_acknowledgements_and_new_claims() {
        let fixture = Fixture::new();
        let mut member = fixture.claim(&[]).unwrap();
        let snapshot = fixture.owner.close().unwrap();
        assert_eq!(snapshot.len(), 1);
        error(member.wait_for_release(), "shutting down");
        error(
            fixture
                .owner
                .poll_joins(|_, _| panic!("closing must not acknowledge a claim")),
            "shutting down",
        );
        error(fixture.claim(&[]), "shutting down");
        assert_eq!(fixture.owner.members().unwrap().len(), 1);
        member.finish().unwrap();
        assert!(fixture.owner.members().unwrap().is_empty());
    }

    #[test]
    fn failed_ack_does_not_release_and_dropped_unstarted_claim_can_restart() {
        let fixture = Fixture::new();
        let member = fixture.claim(&[]).unwrap();
        error(
            fixture
                .owner
                .poll_joins(|_, _| Err(invalid("test release failed"))),
            "test release failed",
        );
        assert!(!fixture.owner.members().unwrap()[0].released);
        drop(member);
        assert!(fixture.owner.members().unwrap().is_empty());
        fixture.claim(&[]).unwrap().finish().unwrap();
    }

    #[test]
    fn stale_claim_can_restart_without_old_token_removing_new_claim() {
        let fixture = Fixture::new();
        let old = fixture.claim(&[]).unwrap();
        fixture.change(|context| context.members[0].wrapper.process_start_time_ticks += 1);
        let mut restarted = fixture.claim(&[]).unwrap();
        drop(old);
        assert_eq!(fixture.owner.members().unwrap().len(), 1);
        fixture.owner.poll_joins(|_, _| Ok(())).unwrap();
        restarted.check_live().unwrap();
        restarted.finish().unwrap();
    }

    #[test]
    fn stale_cleanup_requires_matching_group_supervisor_and_definite_death() {
        let fixture = Fixture::new();
        let context = LockedContext::open(fixture.owner.path()).unwrap().context;
        let mut supervisor = WorkspaceSupervisor {
            pid: context.supervisor.pid,
            process_start_time_ticks: context.supervisor.process_start_time_ticks,
        };
        error(
            cleanup_stale(context.group_id, &supervisor),
            "live or unverifiable",
        );
        supervisor.process_start_time_ticks += 1;
        error(
            cleanup_stale(context.group_id, &supervisor),
            "mismatched group or supervisor",
        );
        fixture.change(|context| context.supervisor.process_start_time_ticks += 1);
        cleanup_stale(context.group_id, &supervisor).unwrap();
        assert!(!fixture.owner.path().exists());
        cleanup_stale(context.group_id, &supervisor).unwrap();
    }

    #[test]
    fn simultaneous_claims_have_exactly_one_winner() {
        let fixture = Fixture::new();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let attempt = || {
                let result = fixture.claim(&[]);
                barrier.wait(); // Keep the winner's claim until both attempted.
                result
            };
            let one = scope.spawn(attempt);
            let two = scope.spawn(attempt);
            let results = [one.join().unwrap(), two.join().unwrap()];
            assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        });
    }

    #[test]
    fn context_and_private_directory_are_removed_with_owner() {
        let fixture = Fixture::new();
        let path = fixture.owner.path().to_owned();
        let directory = path.parent().unwrap().to_owned();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        drop(fixture);
        assert!(!path.exists());
        assert!(!directory.exists());
    }

    #[test]
    #[cfg(unix)]
    fn non_unicode_environment_round_trips_and_insecure_context_is_rejected() {
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::PermissionsExt;
        let mut fixture = Fixture::new();
        let renamed = fixture.root.with_extension(OsString::from_vec(vec![0xff]));
        fs::rename(&fixture.root, &renamed).unwrap();
        fixture.root = renamed;
        let root = fixture.root.clone();
        fixture.change(|context| {
            context.root = root.clone();
            context.apps[0].directory = root.join("app");
            context.state_path = root.join("state.json");
        });
        let value = OsString::from_vec(vec![0xff, b'a']);
        fixture.change(|context| {
            context.apps[0]
                .environment
                .push(("RAW".into(), value.clone()))
        });
        let member = fixture.claim(&[]).unwrap();
        assert_eq!(member.app().environment.last().unwrap().1, value);
        drop(member);
        fs::set_permissions(fixture.owner.path(), fs::Permissions::from_mode(0o644)).unwrap();
        error(fixture.claim(&[]), "private file and directory permissions");
    }

    #[test]
    #[cfg(unix)]
    fn child_is_associated_synchronously_and_remains_owned_after_member_drop() {
        struct ChildGuard(ManagedChild);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.signal(process::ProcessSignal::Kill);
                let _ = self.0.wait();
            }
        }
        let fixture = Fixture::new();
        let member = fixture.claim(&[]).unwrap();
        fixture.owner.poll_joins(|_, _| Ok(())).unwrap();
        let child = ChildGuard(process::spawn_child(&["sleep".into(), "30".into()], &[]).unwrap());
        let mut associated = false;
        member
            .register_child(&child.0, |store, app, supervisor, actual| {
                assert_eq!(actual.pid, child.0.pid);
                assert_eq!(actual.pgid, child.0.pgid);
                assert_eq!(actual.process_start_time_ticks, child.0.start_time_ticks);
                assert_eq!(app.lease_id, actual.id);
                assert_eq!(supervisor.pid, std::process::id());
                store.mutate(|registry| {
                    registry.leases.insert(actual.id, actual.clone());
                    Ok(())
                })?;
                associated = true;
                Ok(())
            })
            .unwrap();
        assert!(associated);
        let claims = fixture.owner.members().unwrap();
        assert_eq!(claims[0].child.as_ref().unwrap().pid, child.0.pid);
        assert_eq!(
            Store::new(fixture.root.join("state.json"))
                .load()
                .unwrap()
                .leases[&member.app().lease_id]
                .pid,
            child.0.pid
        );
        error(
            member.register_child(&child.0, |_, _, _, _| panic!("must not register twice")),
            "unstarted claim",
        );
        drop(member);
        assert_eq!(fixture.owner.members().unwrap().len(), 1);
        error(fixture.claim(&[]), "already claimed");
        fixture.owner.close().unwrap();
        // The shutdown snapshot includes every successfully registered child.
        assert_eq!(
            fixture.owner.members().unwrap()[0]
                .child
                .as_ref()
                .unwrap()
                .pid,
            child.0.pid
        );
    }

    #[test]
    #[cfg(unix)]
    fn atomic_spawn_publishes_before_closing_and_kills_on_association_failure() {
        let fixture = Fixture::new();
        let member = fixture.claim(&[]).unwrap();
        fixture.owner.poll_joins(|_, _| Ok(())).unwrap();
        let mut child = std::thread::scope(|scope| {
            let (spawning, started) = std::sync::mpsc::channel();
            let (resume, continuation) = std::sync::mpsc::channel();
            let (closing, close_started) = std::sync::mpsc::channel();
            let member_ref = &member;
            let launch = scope.spawn(move || {
                member_ref.spawn_child(
                    || {
                        spawning.send(()).unwrap();
                        continuation.recv().unwrap();
                        process::spawn_child(&["sleep".into(), "30".into()], &[])
                            .map_err(|error| process::RunError::from(error).into())
                    },
                    |store, _, _, child| {
                        store.mutate(|registry| {
                            registry.leases.insert(child.id, child.clone());
                            Ok(())
                        })?;
                        Ok(())
                    },
                )
            });
            started.recv().unwrap();
            let owner_ref = &fixture.owner;
            let close = scope.spawn(move || {
                closing.send(()).unwrap();
                owner_ref.close().unwrap()
            });
            close_started.recv().unwrap();
            resume.send(()).unwrap();
            let child = launch.join().unwrap().unwrap();
            let snapshot = close.join().unwrap();
            assert_eq!(snapshot[0].child.as_ref().unwrap().pid, child.pid);
            child
        });
        child.signal(process::ProcessSignal::Kill).unwrap();
        child.wait().unwrap();
        error(member.check_live(), "shutting down");
        error(
            member.spawn_child(|| panic!("must not spawn after close"), |_, _, _, _| Ok(())),
            "shutting down",
        );

        let fixture = Fixture::new();
        let member = fixture.claim(&[]).unwrap();
        fixture.owner.poll_joins(|_, _| Ok(())).unwrap();
        let mut spawned_pid = None;
        error(
            member.spawn_child(
                || {
                    let child = process::spawn_child(&["sleep".into(), "30".into()], &[])
                        .map_err(process::RunError::from)?;
                    spawned_pid = Some(child.pid);
                    Ok(child)
                },
                |_, _, _, _| Err(invalid("association failed")),
            ),
            "association failed",
        );
        assert!(
            process::read_process_identity(spawned_pid.unwrap()).is_none(),
            "failed association must reap the child before returning"
        );
        assert!(fixture.owner.members().unwrap()[0].child.is_none());
    }
}
