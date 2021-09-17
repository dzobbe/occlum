//! This file defines data structures to store contents in bom file
//! as well as all management on these strcutures.
//! Structures `Bom`, `Target`, `SymLink`, `Source`, `NormalFile`, `FileWithOption`
//! are used to parse the bom file.
//! Structures ending with `management` are used to define managements on different levels.
//! We will construct a BomManagement for each bom file (the top bom file and all included bom files).
//! Then do real file operations on each BomManagement
use crate::error::{FILE_NOT_EXISTS_ERROR, INVALID_BOM_FILE_ERROR};
use crate::util::{
    check_file_hash, copy_dir, copy_file, copy_shared_object, create_link, dest_in_root,
    find_dependent_shared_objects, find_included_bom_file, mkdir, resolve_envs,
};
use serde::{Deserialize, Serialize};
use serde_yaml;
use std::collections::{HashSet, VecDeque};
use std::hash::Hash;
use std::path::PathBuf;
use std::slice::Iter;

// The whole bom file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bom {
    pub includes: Option<Vec<String>>,
    pub excludes: Option<Vec<String>>,
    targets: Option<Vec<Target>>,
}

// target in a bom file.
// Each target represents the same destination.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Target {
    target: String,
    mkdirs: Option<Vec<String>>,
    createlinks: Option<Vec<SymLink>>,
    copy: Option<Vec<Source>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymLink {
    src: String,
    linkname: String,
}

// source in a target.
// each Source has the same `from` directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    from: Option<String>,
    dirs: Option<Vec<String>>,
    files: Option<Vec<NormalFile>>,
}

// A file need to be copied
// It can be only a filename; or file with multiple options to enable checking hash, renaming file, etc.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum NormalFile {
    FileName(String),
    FileWithOption(FileWithOption),
}

// A file with multiple optional options
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileWithOption {
    name: String,
    hash: Option<String>,
    autodep: Option<bool>,
    rename: Option<String>,
}

/// all operations defined for one bom file
#[derive(Default)]
pub struct BomManagement {
    dirs_to_make: Vec<String>,
    links_to_create: Vec<(String, String)>,
    dirs_to_copy: Vec<(String, String)>,
    files_to_copy: Vec<(String, String)>,
    shared_objects_to_copy: Vec<(String, String)>,
}

/// all operations defined for one target
#[derive(Default)]
pub struct TargetManagement {
    dirs_to_make: Vec<String>,
    links_to_create: Vec<(String, String)>,
    dirs_to_copy: Vec<(String, String)>,
    files_to_copy: Vec<(String, String)>,
    files_autodep: Vec<String>,
}

/// all operations defined for one source
#[derive(Default)]
pub struct SourceManagement {
    dirs_to_copy: Vec<(String, String)>,
    files_to_copy: Vec<(String, String)>,
    files_autodep: Vec<String>,
}

impl Bom {
    /// This func will manage the root bom file. Find all included bom files, and manage files defined in each bom file
    pub fn manage_top_bom(
        &self,
        bom_file: &str,
        root_dir: &str,
        dry_run: bool,
        included_dirs: &Vec<String>,
    ) {
        // We need to keep the order of boms and bom managements
        let mut sorted_boms = Vec::new();
        let mut bom_managements = Vec::new();
        // find all included boms
        let all_included_boms = find_all_included_bom_files(bom_file, included_dirs);
        for included_bom in all_included_boms.iter() {
            let bom = Bom::from_yaml_file(included_bom);
            sorted_boms.push(bom.clone());
            let bom_management = bom.get_bom_management(root_dir);
            bom_managements.push(bom_management);
        }
        // remove redundant operations in each bom management
        remove_redundant(&mut bom_managements);
        // Since we have different copy options for each bom, we cannot copy all targets together.
        let mut bom_managements_iter = bom_managements.into_iter();
        for bom in sorted_boms.into_iter() {
            // each bom corresponds to a bom management, so the unwrap will never fail
            bom.manage_self(bom_managements_iter.next().unwrap(), dry_run);
        }
    }

    /// This func will only manage the current bom file without finding included bom files
    pub fn manage_self(self, bom_management: BomManagement, dry_run: bool) {
        let excludes = self.excludes.unwrap_or(Vec::new());
        bom_management.manage(dry_run, excludes);
    }

    /// This func will return all operations in one bom
    fn get_bom_management(self, root_dir: &str) -> BomManagement {
        let mut bom_management = BomManagement::default();
        bom_management.dirs_to_make.push(root_dir.to_string()); // init root dir
        if let Some(ref targets) = self.targets {
            for target in targets {
                let target_management = target.get_target_management(root_dir);
                bom_management.add_target_management(target_management, root_dir);
            }
        }
        bom_management
    }

    /// init a bom from a yaml string
    fn from_yaml_string(yaml: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    /// init a bom from a yaml file
    pub fn from_yaml_file(filename: &str) -> Self {
        let file_content = std::fs::read_to_string(filename).unwrap_or_else(|e| {
            error!("cannot read bom file {}. {}.", filename, e);
            std::process::exit(FILE_NOT_EXISTS_ERROR);
        });
        Bom::from_yaml_string(&file_content).unwrap_or_else(|e| {
            error!("{} is not a valid bom file. {}.", filename, e);
            std::process::exit(INVALID_BOM_FILE_ERROR);
        })
    }
}

impl Target {
    fn get_target_management(&self, root_dir: &str) -> TargetManagement {
        let dirs_to_make = self.get_dirs_to_make(root_dir);
        let links_to_create = self.get_links_to_create(root_dir);
        let source_managements = self.get_source_managements(root_dir);
        let mut target_management = TargetManagement::default();
        target_management.dirs_to_make = dirs_to_make;
        target_management.links_to_create = links_to_create;
        for source_management in source_managements.into_iter() {
            target_management.add_source_management(source_management);
        }
        target_management
    }

    fn get_dirs_to_make(&self, root_dir: &str) -> Vec<String> {
        let mut dirs_to_make = Vec::new();
        let target_path = dest_in_root(root_dir, &self.target);
        // mkdir: target path
        dirs_to_make.push(target_path.to_string_lossy().to_string());
        // mkdir: each sub dir
        if let Some(ref dirs) = self.mkdirs {
            for dir in dirs {
                let dir_path = target_path.join(dir);
                dirs_to_make.push(dir_path.to_string_lossy().to_string());
            }
        }
        dirs_to_make
    }

    fn get_links_to_create(&self, root_dir: &str) -> Vec<(String, String)> {
        let target_path = dest_in_root(root_dir, &self.target);
        let mut links_to_create = Vec::new();
        if let Some(ref links) = self.createlinks {
            for link in links {
                let linkname = target_path.join(&link.linkname);
                links_to_create.push((link.src.clone(), linkname.to_string_lossy().to_string()));
            }
        }
        links_to_create
    }

    fn get_source_managements(&self, root_dir: &str) -> Vec<SourceManagement> {
        let target_path = dest_in_root(root_dir, &self.target);
        let mut source_managements = Vec::new();
        if let Some(ref sources) = self.copy {
            let root_dir_path = PathBuf::from(root_dir);
            let workspace_dir = root_dir_path
                .parent()
                .map(|parent| parent.to_string_lossy().to_string())
                .unwrap_or(".".to_string());
            for source in sources {
                let source_management =
                    source.get_source_management(&workspace_dir, &target_path.to_string_lossy());
                source_managements.push(source_management);
            }
        }
        source_managements
    }
}

impl Source {
    fn get_source_management(&self, workspace_dir: &str, target_dir: &str) -> SourceManagement {
        let src_dir = self
            .from
            .as_deref()
            .map(|from| {
                PathBuf::from(workspace_dir)
                    .join(from)
                    .to_string_lossy()
                    .to_string()
            })
            .unwrap_or(workspace_dir.to_string());
        let mut dirs_to_copy = self.get_dirs_to_copy(&src_dir, target_dir);
        let (files_to_copy, files_autodep) =
            self.get_files_to_copy_and_autodep(&src_dir, target_dir);
        // if files and dirs are all None, we will copy the entire `from` directory
        if None == self.files && None == self.dirs {
            let src = self.from.as_deref().unwrap_or_else(|| {
                error!("field 'from' cannot be empty");
                std::process::exit(INVALID_BOM_FILE_ERROR);
            });
            // add the "/" to the directory and will copy the entire directory
            let mut new_src = src.to_string();
            if !new_src.ends_with("/") {
                new_src.push('/');
            }
            dirs_to_copy.push((new_src, target_dir.to_string()));
        }
        SourceManagement {
            dirs_to_copy,
            files_to_copy,
            files_autodep,
        }
    }

    fn get_files_to_copy_and_autodep(
        &self,
        src_dir: &str,
        target_dir: &str,
    ) -> (Vec<(String, String)>, Vec<String>) {
        let mut files_to_copy = Vec::new();
        let mut files_autodep = Vec::new();
        if let Some(ref files) = self.files {
            for file in files {
                let (file_to_copy, file_autodep) =
                    file.get_file_to_copy_and_autodep(src_dir, target_dir);
                files_to_copy.extend(file_to_copy.into_iter());
                files_autodep.extend(file_autodep.into_iter());
            }
        }
        (files_to_copy, files_autodep)
    }

    fn get_dirs_to_copy(&self, src_dir: &str, target_dir: &str) -> Vec<(String, String)> {
        let mut dirs_to_copy = Vec::new();
        if let Some(ref dirs) = self.dirs {
            for dir in dirs {
                let src_path = PathBuf::from(src_dir).join(dir);
                dirs_to_copy.push((
                    src_path.to_string_lossy().to_string(),
                    target_dir.to_string(),
                ));
            }
        }
        dirs_to_copy
    }
}

impl NormalFile {
    fn get_file_to_copy_and_autodep(
        &self,
        src_dir: &str,
        target_dir: &str,
    ) -> (Vec<(String, String)>, Vec<String>) {
        let target_dir_path = PathBuf::from(target_dir);
        let src_dir_path = PathBuf::from(src_dir);
        let mut file_to_copy = Vec::new();
        let mut file_autodep = Vec::new();
        match self {
            NormalFile::FileName(file_name) => {
                let src_file_path = src_dir_path.join(file_name);
                // This unwrap should never fail
                let src_file_name = src_file_path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                let src_file = src_file_path.to_string_lossy().to_string();
                let target_file = target_dir_path
                    .join(src_file_name)
                    .to_string_lossy()
                    .to_string();
                file_to_copy.push((src_file.clone(), target_file));
                // default : autodep is true
                file_autodep.push(src_file);
            }
            NormalFile::FileWithOption(file_with_option) => {
                let file_name = &file_with_option.name;
                let src_file_path = src_dir_path.join(file_name);
                //This unwrap should never fail
                let src_file_name = src_file_path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                let src_file = src_file_path.to_string_lossy().to_string();
                // check file hash
                if let Some(ref hash) = file_with_option.hash {
                    check_file_hash(&src_file, hash);
                }
                // autodep
                if file_with_option.autodep.clone().unwrap_or(true) {
                    file_autodep.push(src_file.clone())
                }
                // rename file
                let target_file = match file_with_option.rename {
                    Some(ref rename) => target_dir_path.join(rename).to_string_lossy().to_string(),
                    None => target_dir_path
                        .join(src_file_name)
                        .to_string_lossy()
                        .to_string(),
                };
                file_to_copy.push((src_file, target_file));
            }
        }
        (file_to_copy, file_autodep)
    }
}

impl BomManagement {
    fn add_target_management(&mut self, mut target_management: TargetManagement, root_dir: &str) {
        // First, we need to resolve environmental variables
        target_management.resolve_environmental_variables();
        let TargetManagement {
            dirs_to_make,
            links_to_create,
            dirs_to_copy,
            files_to_copy,
            files_autodep,
        } = target_management;
        self.dirs_to_make.extend(dirs_to_make.into_iter());
        self.links_to_create.extend(links_to_create.into_iter());
        self.dirs_to_copy.extend(dirs_to_copy.into_iter());
        self.files_to_copy.extend(files_to_copy.into_iter());
        self.autodep(files_autodep, root_dir);
    }

    fn autodep(&mut self, files_autodep: Vec<String>, root_dir: &str) {
        for file_autodep in files_autodep.iter() {
            let mut shared_objects = find_dependent_shared_objects(file_autodep);
            for (src, dest) in shared_objects.drain() {
                let dest_path = dest_in_root(root_dir, &dest);
                // First, we create dir to store the dependency
                // This unwrap should *NEVER* fail
                let dest_dir = dest_path.parent().unwrap().to_string_lossy().to_string();
                self.dirs_to_make.push(dest_dir);
                // Then we copy the dependency to the the dir
                let dest_file = dest_path.to_string_lossy().to_string();
                self.shared_objects_to_copy.push((src, dest_file));
            }
        }
    }

    // do real jobs
    // mkdirs, create links, copy dirs, copy files(including shared objects)
    fn manage(&self, dry_run: bool, excludes: Vec<String>) {
        let BomManagement {
            dirs_to_make,
            links_to_create,
            dirs_to_copy,
            files_to_copy,
            shared_objects_to_copy,
        } = self;
        dirs_to_make.iter().for_each(|dir| mkdir(dir, dry_run));
        links_to_create
            .iter()
            .for_each(|(src, linkname)| create_link(src, linkname, dry_run));
        dirs_to_copy
            .iter()
            .for_each(|(src, dest)| copy_dir(src, dest, dry_run, &excludes));
        files_to_copy
            .iter()
            .for_each(|(src, dest)| copy_file(src, dest, dry_run));
        shared_objects_to_copy
            .iter()
            .for_each(|(src, dest)| copy_shared_object(src, dest, dry_run));
    }
}

impl TargetManagement {
    fn add_source_management(&mut self, source_management: SourceManagement) {
        let SourceManagement {
            dirs_to_copy,
            files_to_copy,
            files_autodep,
        } = source_management;
        self.dirs_to_copy.extend(dirs_to_copy.into_iter());
        self.files_to_copy.extend(files_to_copy.into_iter());
        self.files_autodep.extend(files_autodep.into_iter());
    }

    fn resolve_environmental_variables(&mut self) {
        self.dirs_to_make = self
            .dirs_to_make
            .iter()
            .map(|dir| resolve_envs(dir))
            .collect();
        self.links_to_create = self
            .links_to_create
            .iter()
            .map(|(src, linkname)| (resolve_envs(src), resolve_envs(linkname)))
            .collect();
        self.dirs_to_copy = self
            .dirs_to_copy
            .iter()
            .map(|(src, dest)| (resolve_envs(src), resolve_envs(dest)))
            .collect();
        self.files_to_copy = self
            .files_to_copy
            .iter()
            .map(|(src, dest)| (resolve_envs(src), resolve_envs(dest)))
            .collect();
        self.files_autodep = self
            .files_autodep
            .iter()
            .map(|file| resolve_envs(file))
            .collect();
    }
}

/// This function will return all included bom files in the order to deal with.
/// This function operates in such a way: It starts from putting the root bom into a queue,
/// In each iteration of the loop, it will fetch the first bom from the head of the queue,
/// Then it find all included files of the bom file. The all included bom files will put into the queue as well as a vector(sorted boms).
/// The loop will end if there's no more elements in the queue.
/// There is also a max_iteration bound. If the loop exceeds the bound and the queue is not empty, the function will abort the program.
/// Because excess of the bound often means there's a reference cycles in the bom tree, which is an invalid case.
/// After we visit all boms in the queue, we will get all boms sorted in the order of being included in the vector.
/// Then we will remove redudant boms in the vector. For a bom file that may exist more than one time,
/// only the last one will be kept in the final result. To remove redundancy, we will reverse the vector,
/// and only keep the first one for each duplicate bom.
fn find_all_included_bom_files(bom_file: &str, included_dirs: &Vec<String>) -> Vec<String> {
    let mut boms = VecDeque::new();
    let mut sorted_boms = Vec::new();
    const MAX_ITERATION: usize = 100;

    boms.push_back(bom_file.to_string());
    sorted_boms.push(bom_file.to_string());
    for _ in 0..MAX_ITERATION {
        if boms.is_empty() {
            break;
        }
        // This unwrap can never fail
        let current_bom = boms.pop_front().unwrap();
        let bom = Bom::from_yaml_file(&current_bom);
        // find includes for current bom
        if let Some(includes) = bom.includes {
            includes.into_iter().for_each(|include| {
                let included_bom_file =
                    find_included_bom_file(&include, &current_bom, included_dirs);
                boms.push_back(included_bom_file.clone());
                sorted_boms.push(included_bom_file);
            });
        }
    }
    if !boms.is_empty() {
        // The iteration exceeds the MAX_ITERATION and there still are elements in the queue.
        error!("The bom file number exceeds the MAX_ITERATION bound. Please check if there is including cycle.");
        std::process::exit(INVALID_BOM_FILE_ERROR);
    }
    // remove redundant boms in sorted boms
    sorted_boms.reverse();
    let mut res = remove_redundant_items_in_vec(&sorted_boms, Vec::new().iter());
    res.reverse();
    res
}

// remove redundant items in a vec. For duplicate items, only the first item will be reserved
fn remove_redundant_items_in_vec<T>(raw: &Vec<T>, excludes: Iter<'_, T>) -> Vec<T>
where
    T: Hash + Eq + Clone,
{
    let mut exists = HashSet::new();
    for item in excludes {
        exists.insert(item.clone());
    }
    let mut res = Vec::new();
    for item in raw {
        if exists.insert(item.clone()) {
            res.push(item.clone());
        }
    }
    res
}
