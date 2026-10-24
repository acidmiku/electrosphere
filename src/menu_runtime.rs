//! Host wiring around the separately validated original menu callbacks.
use crate::{
    Result,
    assets::GameData,
    menu_scene::{LanguageScene, MenuFrame, MenuVisualState},
};
use ac3_menu::*;
use serde_json::{Value, json};

pub struct MenuRuntime<'a> {
    scene: LanguageScene<'a>,
    visual: MenuVisualState,
    tables: MenuTables,
    pub menu: MenuState,
    pub context: MenuContext,
    context_bytes: Vec<u8>,
    language: LanguageState,
    title: TitleState,
    main: MainMenuState,
    difficulty: DifficultyState,
    exit: ExitState,
    pub pending: Option<&'static str>,
}
impl<'a> MenuRuntime<'a> {
    pub fn game_context_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = self.context_bytes.clone();
        self.context.write_into(&mut bytes)?;
        Ok(bytes)
    }

    pub fn new(data: &'a GameData) -> Result<Self> {
        let scene = LanguageScene::load(data)?;
        let mut visual = scene.visual_state()?;
        let update = initialize_language(MenuContext::default());
        visual.apply(&update.effects, 0)?;
        Ok(Self {
            scene,
            visual,
            tables: MenuTables::from_overlay(data.overlay(1)?)?,
            menu: MenuState {
                state: 1,
                substate: 1,
                ..Default::default()
            },
            context: update.context,
            context_bytes: vec![0; 0xf70],
            language: update.state,
            title: TitleState::default(),
            main: MainMenuState::default(),
            difficulty: DifficultyState::default(),
            exit: ExitState::default(),
            pending: None,
        })
    }

    pub fn step(
        &mut self,
        frame_counter: i32,
        pressed: u32,
        repeated: u32,
    ) -> Result<(MenuFrame, Value)> {
        let before = self.menu;
        let (result, mut effects) = match (self.menu.state, self.menu.substate) {
            (1, 1) => {
                let u = tick_language(self.language, self.context, pressed)?;
                self.language = u.state;
                self.context = u.context;
                (u.callback_result, u.effects)
            }
            (2, 0) => {
                let u = initialize_title(self.context);
                self.title = u.state;
                self.context = u.context;
                (u.callback_result, u.effects)
            }
            (2, 1) => {
                let u = tick_title(self.title, self.context, pressed)?;
                self.title = u.state;
                self.context = u.context;
                (u.callback_result, u.effects)
            }
            (3, 0) => {
                let u = initialize_main_menu(self.menu, self.context);
                self.main = u.state;
                self.menu = u.menu;
                self.context = u.context;
                (u.callback_result, u.effects)
            }
            (3, 1) => {
                let u = tick_main_menu(self.main, self.menu, self.context, pressed, repeated);
                self.main = u.state;
                self.menu = u.menu;
                self.context = u.context;
                (u.callback_result, u.effects)
            }
            (4, 0) => {
                let u = initialize_difficulty(self.difficulty, self.context);
                self.difficulty = u.state;
                self.context = u.context;
                (u.callback_result, u.effects)
            }
            (4, 1) => {
                let u = tick_difficulty(self.difficulty, self.context, pressed)?;
                self.difficulty = u.state;
                self.context = u.context;
                (u.callback_result, u.effects)
            }
            (6, 0) => {
                let (state, result, mut effects) = initialize_exit();
                self.exit = state;
                effects.push(Effect::ClearTextVisibility);
                (result, effects)
            }
            (6, 1) => {
                let (state, context, result, effects) = tick_exit(self.exit, self.context);
                self.exit = state;
                self.context = context;
                (result, effects)
            }
            pair => return Err(format!("menu callback {pair:?} is not connected").into()),
        };
        self.context.write_into(&mut self.context_bytes)?;
        if effects.iter().any(|e| {
            matches!(
                e,
                Effect::PendingOriginalCall {
                    address: 0x800d1800,
                    ..
                }
            )
        }) {
            effects.extend(title_progress_effects(&self.context_bytes)?);
        }
        for effect in &effects {
            match *effect {
                Effect::SetContextByte { offset, value } => {
                    *self
                        .context_bytes
                        .get_mut(offset as usize)
                        .ok_or("context byte outside host allocation")? = value
                }
                Effect::SetContextWord { offset, value } => self
                    .context_bytes
                    .get_mut(offset as usize..offset as usize + 4)
                    .ok_or("context word outside host allocation")?
                    .copy_from_slice(&value.to_le_bytes()),
                Effect::ResetNewGameProgress => reset_new_game_progress(&mut self.context_bytes)?,
                _ => (),
            }
        }
        let post =
            apply_menu_callback_result(self.menu, self.context, result as i32, &self.tables)?;
        self.menu = post.state;
        self.context = post.context;
        let image = self.scene.render_menu(
            &mut self.visual,
            self.context.language_id as usize,
            frame_counter,
            &effects,
        )?;
        if post.callback_result == 0x60000003 {
            self.pending = Some("mission_transition_pending");
        } else if post.callback_result != 0 {
            self.pending = Some("resident_transition_pending");
        } else if self.menu.state == 5 {
            self.pending = Some("load_game_pending");
        }
        let sounds: Vec<u32> = effects
            .iter()
            .filter_map(|e| {
                if let Effect::PlaySound { id } = e {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect();
        let trace = json!({"frame":frame_counter,"input":pressed,"repeat":repeated,
            "menu_state_before":before.state,"menu_substate_before":before.substate,"menu_state":self.menu.state,
            "menu_substate":self.menu.substate,"language_phase":self.language.phase,"language_timer":self.language.timer,
            "language_selection":self.language.selection,"title_phase":self.title.phase,"title_timer":self.title.timer,
            "main_selection":self.main.selection,"difficulty_phase":self.difficulty.phase,"difficulty_selection":self.difficulty.selection,
            "language_id":self.context.language_id,"result":result,"resident_result":post.callback_result,"sound_requests":sounds});
        Ok((image, trace))
    }
}
