//! Ways of coloring meters of the user's own, kept beside Weir's own
//! Default the way equalizer presets are kept.

use super::handlers::NAME_MAX;
use super::Controller;
use crate::config;
use serde_json::Value;
use tracing::{debug, info};
use weir_protocol::*;

/// `c` as it is kept: a trimmed name, colors written alike. Refused when
/// the name is taken by Weir's own, or anything about it is wrong.
pub(super) fn checked_meter_colors(c: &MeterColors) -> Result<MeterColors, RpcError> {
    let name = c.name.trim().to_string();
    if name.is_empty() || name.chars().count() > NAME_MAX {
        return Err(RpcError::invalid_params(format!(
            "a name for meter colors needs 1 to {NAME_MAX} characters"
        )));
    }
    if name.eq_ignore_ascii_case(METER_COLORS_DEFAULT) {
        return Err(RpcError::application(format!(
            "'{METER_COLORS_DEFAULT}' is Weir's own; choose another name"
        )));
    }
    if let Some(problem) = c.problem() {
        return Err(RpcError::invalid_params(format!(
            "these meter colors will not do: {problem}"
        )));
    }
    Ok(MeterColors {
        name,
        colors: c.rgb().into_iter().map(hex_color).collect(),
        breaks_db: c.breaks_db.clone(),
        builtin: false,
    })
}

impl Controller {
    /// Weir's own way of coloring meters first, then the user's.
    pub(super) fn meter_colors(&self) -> Vec<MeterColors> {
        let mut all = vec![MeterColors::weir_default()];
        if let Ok(user) = &self.inner.lock().unwrap().user_meter_colors {
            all.extend(user.iter().cloned());
        }
        all
    }

    /// Change the user's meter colors with `f`, write them out and tell
    /// clients.
    pub(super) fn edit_meter_colors(
        &self,
        f: impl FnOnce(&mut Vec<MeterColors>) -> Result<(), RpcError>,
    ) -> Result<Value, RpcError> {
        {
            let mut inner = self.inner.lock().unwrap();
            let user = inner.user_meter_colors.as_mut().map_err(|e| {
                RpcError::application(format!(
                    "meter colors are read-only until this is fixed: {e}"
                ))
            })?;
            let mut candidate = user.clone();
            f(&mut candidate)?;
            candidate.sort_by_key(|c| c.name.to_lowercase());
            config::save_meter_colors(&self.paths.meter_colors_file, &candidate)
                .map_err(|e| RpcError::application(format!("{e:#}")))?;
            *user = candidate;
        }
        let all = self.meter_colors();
        self.announce(Notification::MeterColorsChanged(all.clone()));
        Ok(to_json(&all))
    }

    pub(super) fn save_meter_colors(&self, c: MeterColors) -> Result<Value, RpcError> {
        let c = checked_meter_colors(&c)?;
        let name = c.name.clone();
        let result = self.edit_meter_colors(|user| {
            match user
                .iter_mut()
                .find(|u| u.name.eq_ignore_ascii_case(&c.name))
            {
                Some(existing) => *existing = c,
                None => user.push(c),
            }
            Ok(())
        })?;
        debug!("saved meter colors '{name}'");
        Ok(result)
    }

    pub(super) fn delete_meter_colors(&self, p: NameParams) -> Result<Value, RpcError> {
        let name = p.name.trim().to_string();
        if name.eq_ignore_ascii_case(METER_COLORS_DEFAULT) {
            return Err(RpcError::application(format!(
                "'{METER_COLORS_DEFAULT}' is Weir's own and cannot be deleted"
            )));
        }
        let result = self.edit_meter_colors(|user| {
            let before = user.len();
            user.retain(|u| !u.name.eq_ignore_ascii_case(&name));
            if user.len() == before {
                return Err(RpcError::application(format!(
                    "no meter colors called '{name}'"
                )));
            }
            Ok(())
        })?;
        info!("deleted meter colors '{name}'");
        Ok(result)
    }
}
