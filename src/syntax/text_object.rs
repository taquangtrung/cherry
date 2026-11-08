//! Structural text objects: `af` and `if` (a function, its body), `ac` and
//! `ic` (a class or type, its body), registered as host commands through
//! Cherry's public command API and answered by the document's
//! highlighter.

use crate::{CommandContext, CommandError, Editor, Mode};

use super::highlighter::{CLASS_OBJECT, FUNCTION_OBJECT};

// ========================================================================
// Constants
// ========================================================================

/// Each text object: its command id, its keys, the object, and whether it
/// is the whole object.
const TEXT_OBJECTS: &[(&str, &str, &str, bool)] = &[
    ("syntax-around-class", "a c", CLASS_OBJECT, true),
    ("syntax-around-function", "a f", FUNCTION_OBJECT, true),
    ("syntax-inner-class", "i c", CLASS_OBJECT, false),
    ("syntax-inner-function", "i f", FUNCTION_OBJECT, false),
];

/// The modes text objects are bound in.
const OBJECT_MODES: [Mode; 3] = [Mode::OperatorPending, Mode::Visual, Mode::VisualLine];

// ========================================================================
// Functions
// ========================================================================

/// Registers `af`, `if`, `ac`, and `ic` in `editor`, in operator-pending
/// and the visual modes: after an operator they act on the function or
/// class around the cursor (the count-th enclosing one), in a visual mode
/// they select it. They ask the document's highlighter, so they find
/// nothing (cancelling an operator) in a document with none, or in a
/// language with no such structure. Fails when the command ids
/// (`syntax-around-function`, ...) are taken, as when called twice.
pub fn register_text_objects(editor: &mut Editor) -> Result<(), CommandError> {
    for (id, keys, object, around) in TEXT_OBJECTS {
        let (object, around) = (*object, *around);
        let run = move |cx: &mut CommandContext<'_>| {
            let count = cx.count_or_one();
            let found = cx.editor.view(cx.view).and_then(|view| {
                let document = cx.editor.document(view.document())?;
                let highlighter = cx.editor.highlighter(view.document())?;
                highlighter.text_object(document, object, around, view.cursor(), count)
            });
            cx.act_on_text_object(found);
        };
        editor.register_command(*id, run)?;
        for mode in OBJECT_MODES {
            editor.bind(mode, &[keys], id)?;
        }
    }
    Ok(())
}
