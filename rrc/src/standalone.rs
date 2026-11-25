//! `rrc standalone` (D226): the book a port played on its own starts from power-on by (`standalone.json`, the
//! runtime's `standalone::Book`): where each mapped and carried field's initial value is in the program's load image
//! and its type, the inputs the start-up finds in the image, the program's file and the versions it is of, the
//! arguments the port's runs give it. From the mapping and the knowledge alone: nothing of the game is in it. `rrc
//! check` refuses a book behind them; the initial data `rrc start` and a comparison give a port are read by it too,
//! so the two are the same.

use std::process::ExitCode;

use rrc_runtime::standalone::{Array, Book, FILE, Input, Item, VERSION};
use rrc_types::values::Arrays;

use crate::args::Args;
use crate::knowledge::Knowledge;
use crate::mapping::{Mapped, Mapping};
use crate::repo::Repo;
use crate::say;

/// Whether `id` is at a fixed place in `module`'s load image (not in an overlay or an object).
fn in_image(module: &str, id: &ids::Id) -> bool {
    id.module() == module && id.part().is_none()
}

/// The book's items of `items` (the mapped and carried fields): those at fixed places in `module`'s load image, not
/// found through a pointer nor an index.
pub(crate) fn initial_items(module: &str, items: &[Mapped]) -> Vec<Item> {
    items
        .iter()
        .filter(|m| m.via.is_none() && m.into.is_none() && in_image(module, &m.id))
        .map(|m| Item {
            field: m.field.clone(),
            offset: m.id.offset(),
            size: m.size,
            ty: m.ty.clone(),
            mask: m.mask,
            set: m.set,
        })
        .collect()
}

/// The named arrays the knowledge's pointer types are read into.
pub(crate) fn arrays(k: &Knowledge) -> Vec<Array> {
    k.types
        .pointers
        .iter()
        .filter(|p| !p.into.is_empty())
        .filter_map(|p| {
            let (id, element, count) = k.array(&p.into)?;
            Some(Array { name: p.into.clone(), id: id.to_string(), element, count })
        })
        .collect()
}

/// The book of `items` (the mapped and carried fields) and the knowledge `k`.
pub(crate) fn book(repo: &Repo, k: &Knowledge, items: &[Mapped]) -> Book {
    let module = repo.module();
    let items = initial_items(&module, items);
    let inputs = k
        .ticks
        .inputs
        .iter()
        .filter(|i| i.device.is_empty() && i.part.is_empty())
        .filter_map(|i| {
            let id: ids::Id = i.at.parse().ok().filter(|id| in_image(&module, id))?;
            Some(Input { name: i.name.clone(), offset: id.offset(), size: i.size })
        })
        .collect();
    let file = repo.module_file();
    let program = file.rsplit(['/', '\\']).next().unwrap_or(&file).to_owned();
    let versions = repo
        .settings
        .versions
        .iter()
        .filter_map(|v| v.files.iter().find(|(path, _)| path.eq_ignore_ascii_case(&file)).map(|(_, hash)| hash.clone()))
        .collect();
    let options = &repo.settings.original.options;
    let args = options.iter().position(|o| o == "--args").and_then(|i| options.get(i + 1)).cloned().unwrap_or_default();
    Book {
        format: "rrc-standalone".into(),
        version: VERSION,
        program,
        versions,
        args,
        types: k.types.clone(),
        arrays: arrays(k),
        items,
        inputs,
    }
}

/// The book as the mapping and the knowledge have it now.
pub(crate) fn current(repo: &Repo) -> Result<Book, String> {
    let k = Knowledge::load(&repo.knowledge())?;
    let mapping = Mapping::load(repo)?;
    let mut items = mapping.resolve(&k)?;
    items.extend(mapping.resolve_carried(&k)?);
    Ok(book(repo, &k, &items))
}

/// `rrc standalone`: writes the book.
pub(crate) fn command(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &[], &[])?;
    a.positionals(0, 0, "standalone")?;
    let book = current(repo)?;
    let path = repo.root.join(FILE);
    std::fs::write(&path, book.text()).map_err(|e| format!("{}: {e}", path.display()))?;
    say(
        "book",
        format!(
            "{FILE}: {} fields' initial data, {} inputs in the image, the program {} ({} versions), its arguments",
            book.items.len(),
            book.inputs.len(),
            book.program,
            book.versions.len()
        ),
    );
    say("next", "the port builds it in and plays on its own from power-on (rrc_platform's Live::STANDALONE)");
    Ok(ExitCode::SUCCESS)
}

/// A problem for `rrc check`: a book behind the mapping and the knowledge (none where there is no book).
pub(crate) fn problem(repo: &Repo) -> Result<Option<String>, String> {
    let path = repo.root.join(FILE);
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(None) };
    let fresh = current(repo)?;
    Ok(match Book::read(&text) {
        Ok(book) if book == fresh => None,
        _ => Some(format!("{FILE} is behind the mapping and the knowledge: rrc standalone writes it again (D226)")),
    })
}
