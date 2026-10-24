//! Native resource-loader and first-hub scheduling. Resource data is already
//! verified and resident in GameData; read completion is therefore available on
//! the next poll, while TIM batches retain their original per-callback limits.
use crate::{
    Result,
    aircraft_scene::AircraftScene,
    assets::GameData,
    briefing_scene::BriefingRenderer,
    flight_loader::{InactiveTrail, initialize_inactive_trails},
    menu_scene::{LanguageScene, MenuFrame},
    mission_assets::{MissionAssets, MissionTextureQueue},
    mission_background::PaletteGradient,
    mission_hub::{HubLayers, HubScene},
    mission_map::Random,
    mission_math::NativeBriefingMath,
};
use ac3_formats::dot1;
use ac3_mission::*;
use serde_json::{Value, json};

pub struct MissionRuntime<'a> {
    data: &'a GameData,
    pub scene: HubScene<'a>,
    pub state: MissionState,
    pub context: MissionContext,
    pub loader: MissionLoaderState,
    pub hub: HubState,
    pub briefing: FullBriefingState,
    pub aircraft: AircraftSelectionState,
    pub launch: LaunchState,
    pub pending: Option<&'static str>,
    pub random: Random,
    pub inactive_trails: [InactiveTrail; 3],
    context_bytes: Vec<u8>,
    language: usize,
    tables: AircraftTables,
    queue: Option<MissionTextureQueue<'a>>,
    read_resource: Option<u32>,
    briefing_renderer: BriefingRenderer<'a>,
    briefing_script: BriefingScript,
    math: NativeBriefingMath,
    aircraft_renderer: AircraftScene<'a>,
}

impl<'a> MissionRuntime<'a> {
    pub fn new(data: &'a GameData, mut context_bytes: Vec<u8>, language: usize) -> Result<Self> {
        let initialized = initialize_mission(MissionContext::from_bytes(&context_bytes)?);
        if initialized.state.state != 14 {
            return Err("native mission runtime currently requires the new-game loader".into());
        }
        initialized.context.write_into(&mut context_bytes)?;
        let tables = AircraftTables::from_overlay(data.overlay(2)?)?;
        let resource = loader_mission_resource(LoaderContext::from_bytes(&context_bytes)?, &tables);
        let mut random = Random { state: 0 };
        let mut scene = HubScene::new(
            data,
            MissionAssets::load(data, resource as usize)?,
            &mut random,
        )?;
        let inactive_trails = initialize_inactive_trails(&mut random);
        scene.visual = scene.scene.blank_visual_state();
        // The source data can be parsed eagerly, but textures become visible
        // only when the translated loading queue submits their uploads.
        scene.scene.vram = LanguageScene::load(data)?.vram;
        Ok(Self {
            data,
            scene,
            state: initialized.state,
            context: initialized.context,
            loader: MissionLoaderState::default(),
            hub: HubState::default(),
            briefing: FullBriefingState::default(),
            aircraft: AircraftSelectionState::default(),
            launch: LaunchState::default(),
            pending: None,
            random,
            inactive_trails,
            context_bytes,
            language,
            tables,
            queue: None,
            read_resource: None,
            briefing_renderer: BriefingRenderer::new(data, resource as usize)?,
            briefing_script: BriefingScript::parse(
                dot1::parse(data.resource(resource as usize)?)?.child(1)?,
            )?,
            math: NativeBriefingMath::load(data)?,
            aircraft_renderer: AircraftScene::load(data)?,
        })
    }

    pub fn game_context_bytes(&self) -> &[u8] {
        &self.context_bytes
    }

    pub fn step(&mut self, frame: i32, input: u32, held_input: u32) -> Result<(MenuFrame, Value)> {
        if self.state.state == 1 {
            return self.step_briefing(frame, input, held_input);
        }
        if self.state.state == 2 {
            return self.step_aircraft(frame, input, held_input);
        }
        let before = self.state;
        let mut effects = Vec::new();
        let mut progress_bar = None;
        let mut load_mission_sound_bank = false;
        let mut initialize_background_voices = false;
        let mut upload_count = 0;
        let callback_result;
        if self.state.state == 14 {
            let loader_context = LoaderContext::from_bytes(&self.context_bytes)?;
            let update = if self.state.substate == 0 {
                initialize_mission_loader(self.loader, loader_context)
            } else {
                let resource = loader_mission_resource(loader_context, &self.tables);
                let mut inputs = LoaderInputs {
                    mission_resource_bytes: i32::try_from(
                        self.data.resource(resource as usize)?.len(),
                    )?,
                    common_resource_bytes: i32::try_from(self.data.resource(72)?.len())?,
                    ..Default::default()
                };
                match loader_requirement(self.loader)? {
                    LoaderRequirement::TransferProgress => {
                        let active = self
                            .read_resource
                            .ok_or("loader polled without a resource read")?;
                        inputs.transferred_total_bytes =
                            i32::try_from(self.data.resource(active as usize)?.len())?
                                .wrapping_add(if active == 72 {
                                    self.loader.mission_bytes
                                } else {
                                    0
                                });
                        inputs.read_complete = true;
                    }
                    LoaderRequirement::TextureQueuePoll => {
                        let result = self
                            .queue
                            .as_mut()
                            .ok_or("loader polled without a texture queue")?
                            .poll(&mut self.scene.scene.vram)?;
                        inputs.texture_poll_result = i32::from(result.complete);
                        upload_count = result.uploads.len();
                    }
                    _ => (),
                }
                tick_mission_loader(self.loader, loader_context, inputs, &self.tables)?
            };
            self.loader = update.state;
            update.context.write_into(&mut self.context_bytes)?;
            self.context = MissionContext::from_bytes(&self.context_bytes)?;
            callback_result = update.callback_result;
            for effect in update.effects {
                match effect {
                    LoaderEffect::Common(effect) => effects.push(effect),
                    LoaderEffect::DrawProgressBar { width } => {
                        progress_bar =
                            Some(vec![0x60d5e5e5, 0x01a400c0, 0x00060000 | u32::from(width)]);
                    }
                    LoaderEffect::BindBuiltinScripts { address } => {
                        if address != 0x800f1e50 {
                            return Err("unknown builtin loader script table".into());
                        }
                        self.scene.scene.use_mission_loading_scripts(true)?;
                    }
                    LoaderEffect::BeginResourceRead { resource } => {
                        self.data.resource(resource as usize)?;
                        self.read_resource = Some(resource);
                    }
                    LoaderEffect::BindMissionResource {
                        resource,
                        texture_child,
                        batch_size,
                    }
                    | LoaderEffect::BindCommonResource {
                        resource,
                        texture_child,
                        batch_size,
                    } => {
                        let container = dot1::parse(self.data.resource(resource as usize)?)?;
                        self.queue = Some(MissionTextureQueue::new(
                            container.child(usize::from(texture_child))?,
                            usize::try_from(batch_size)?,
                        )?);
                    }
                    LoaderEffect::LoadSoundBank {
                        resource: 72,
                        child: 9,
                    } => load_mission_sound_bank = true,
                    LoaderEffect::LoadSoundBank { .. } => {
                        return Err("unknown mission sound bank".into());
                    }
                    LoaderEffect::InitializeBackgroundVoices => initialize_background_voices = true,
                    LoaderEffect::SnapshotMapPalette => {
                        self.scene.map.palette = PaletteGradient::from_vram(
                            &self.scene.scene.vram,
                            self.scene.map.tube_texture.clut,
                        )?;
                    }
                    LoaderEffect::BindCommonScripts {
                        resource: 72,
                        child: 3,
                    } => {
                        self.scene.scene.use_mission_loading_scripts(false)?;
                    }
                    LoaderEffect::BindCommonScripts { .. } => {
                        return Err("unknown common script table".into());
                    }
                    LoaderEffect::ReleaseResourceChild { .. } => {
                        // GameData owns immutable source bytes. No guest heap is allocated.
                    }
                }
            }
        } else if self.state.state == 0 || self.state.state == 4 {
            let update = if self.state.substate == 0 {
                initialize_hub(self.state, self.context)
            } else {
                tick_hub(self.hub, self.state, self.context, input)?
            };
            self.hub = update.state;
            self.state = update.mission;
            self.context = update.context;
            callback_result = update.callback_result;
            effects = update.effects;
        } else if self.state.state == 3 {
            let update = if self.state.substate == 0 {
                initialize_launch(self.context)
            } else {
                tick_launch(self.launch, self.context, input)
            };
            self.launch = update.state;
            self.context = update.context;
            callback_result = update.callback_result;
            effects = update.effects;
        } else {
            return Err("mission callback is not connected".into());
        }
        let post = apply_callback_result(self.state, self.context, callback_result)?;
        self.state = post.state;
        self.context = post.context;
        self.context.write_into(&mut self.context_bytes)?;
        effects.extend(post.effects);
        let mut layers = self
            .scene
            .layers(&effects, frame, self.language, held_input)?;
        if let Some(packet) = progress_bar {
            layers.packets.push(packet);
        }
        let depth = self.scene.map.tick(self.scene.color.rgb)?;
        let depth: Vec<_> = depth
            .execution_order()
            .iter()
            .map(|p| p.words.clone())
            .collect();
        let image = self.scene.render(&layers, &depth)?;
        if post.callback_result != 0 {
            self.pending = Some("mission_resident_transition_pending");
        } else if ![0, 1, 2, 3, 4, 14].contains(&self.state.state) {
            self.pending = Some(match self.state.state {
                1 => "briefing_rendering_pending",
                2 => "aircraft_rendering_pending",
                7 | 8 => "records_pending",
                9 => "save_pending",
                10 => "options_pending",
                _ => "mission_callback_pending",
            });
        }
        let sounds: Vec<_> = effects
            .iter()
            .filter_map(|effect| {
                if let Effect::PlaySound { id } = effect {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect();
        let audio_background_fade = effects
            .iter()
            .any(|e| matches!(e, Effect::BeginHubBackgroundFade));
        let launch_background_fade = effects
            .iter()
            .any(|e| matches!(e, Effect::BeginLaunchBackgroundFade));
        let unhandled: Vec<_> = layers
            .remaining_effects
            .iter()
            .filter(|e| {
                !matches!(
                    e,
                    Effect::PlaySound { .. }
                        | Effect::BeginHubBackgroundFade
                        | Effect::BeginLaunchBackgroundFade
                )
            })
            .map(|e| format!("{e:?}"))
            .collect();
        let trace = json!({"frame":frame,"input":input,"mission_state_before":before.state,
            "mission_substate_before":before.substate,"mission_state":self.state.state,"mission_substate":self.state.substate,
            "result":callback_result,"resident_result":post.callback_result,"loader_phase":self.loader.phase,
            "loader_substate":self.loader.substate,"loader_total_bytes":self.loader.total_bytes,
            "loader_loaded_bytes":self.loader.loaded_bytes,"texture_uploads":upload_count,
            "hub_phase":self.hub.phase,"hub_selection":self.hub.selection,"map_frame":self.scene.map.state.frame,
            "sound_requests":sounds,"load_mission_sound_bank":load_mission_sound_bank,
            "initialize_background_voices":initialize_background_voices,"begin_hub_background_fade":audio_background_fade,
            "begin_launch_background_fade":launch_background_fade,"launch_timer":self.launch.timer,
            "unhandled_effects":unhandled});
        Ok((image, trace))
    }

    fn step_briefing(
        &mut self,
        frame: i32,
        input: u32,
        held_input: u32,
    ) -> Result<(MenuFrame, Value)> {
        let before = self.state;
        let clear_color = self.scene.begin_frame(held_input)?;
        let update = if self.state.substate == 0 {
            initialize_briefing(
                self.briefing,
                &self.briefing_script,
                i32::try_from(self.briefing_renderer.ui.text_line_count(self.language)?)?,
                &self.math,
            )?
        } else {
            prepare_briefing_tick(
                self.briefing,
                &self.briefing_script,
                BriefingFrameInput {
                    pressed: input,
                    visual_address: 0,
                    visual_offset2: self.scene.visual.offset(2)?,
                },
                &self.math,
            )?
        };
        self.scene.visual.set_frame_color(self.scene.color.rgb);
        let commands = self.briefing_renderer.commands(
            &self.scene.scene,
            &mut self.scene.visual,
            &update,
            self.language,
            self.context.current_pair[0] as i16,
        )?;
        let update = finish_briefing_render(update, &commands.outcomes)?;
        self.briefing = update.state;
        let post = apply_callback_result(self.state, self.context, update.callback_result)?;
        self.state = post.state;
        self.context = post.context;
        self.context.write_into(&mut self.context_bytes)?;
        let mut sounds = Vec::new();
        let mut unhandled = Vec::new();
        let mut briefing_fade = false;
        let mut resume_fade = false;
        let mut stream = None;
        for effect in &commands.remaining_effects {
            match effect {
                BriefingEffect::Common(Effect::PlaySound { id }) => sounds.push(*id),
                BriefingEffect::Common(Effect::PendingOriginalCall {
                    address: 0x800e983c,
                }) => self.scene.map.set_object_phase(3)?,
                BriefingEffect::Common(Effect::PendingOriginalCall {
                    address: 0x800e9824,
                }) => self.scene.map.set_object_phase(1)?,
                BriefingEffect::Common(Effect::PendingOriginalCallWithArgs {
                    address: 0x800e7a18,
                    arguments,
                }) if arguments == &[30, 10] => briefing_fade = true,
                BriefingEffect::Common(Effect::PendingOriginalCallWithArgs {
                    address: 0x800e7a18,
                    arguments,
                }) if arguments == &[100, 10] => resume_fade = true,
                BriefingEffect::Common(Effect::StartStream { resource }) => {
                    stream = Some(json!({"action":"start","resource":resource}));
                }
                BriefingEffect::Common(Effect::StopStream) => {
                    stream = Some(json!({"action":"stop"}));
                }
                _ => unhandled.push(format!("{effect:?}")),
            }
        }
        unhandled.extend(post.effects.iter().map(|e| format!("{e:?}")));
        let depth = self.scene.map.tick(self.scene.color.rgb)?;
        let depth: Vec<_> = depth
            .execution_order()
            .iter()
            .map(|p| p.words.clone())
            .collect();
        let layers = HubLayers {
            packets: commands.execution_order(),
            clear_color,
            remaining_effects: Vec::new(),
        };
        let image = self.scene.render(&layers, &depth)?;
        if post.callback_result != 0 {
            self.pending = Some("mission_resident_transition_pending");
        } else if ![0, 1, 2, 4].contains(&self.state.state) {
            self.pending = Some("mission_callback_pending");
        }
        let trace = json!({"frame":frame,"input":input,"mission_state_before":before.state,"mission_substate_before":before.substate,
            "mission_state":self.state.state,"mission_substate":self.state.substate,"result":update.callback_result,"resident_result":post.callback_result,
            "briefing_phase":self.briefing.control.phase,"briefing_substate":self.briefing.control.substate,
            "briefing_script_frame":self.briefing.script.clock,"map_frame":self.scene.map.state.frame,"texture_uploads":0,
            "sound_requests":sounds,"begin_briefing_background_fade":briefing_fade,"resume_background_fast":resume_fade,
            "stream_request":stream,"unhandled_effects":unhandled});
        Ok((image, trace))
    }

    fn step_aircraft(
        &mut self,
        frame: i32,
        input: u32,
        held_input: u32,
    ) -> Result<(MenuFrame, Value)> {
        let before = self.state;
        let clear_color = self.scene.begin_frame(held_input)?;
        let context = AircraftContext::from_bytes(&self.context_bytes)?;
        let update = if self.state.substate == 0 {
            initialize_aircraft(self.aircraft, context)
        } else {
            tick_aircraft(self.aircraft, context, &self.tables, input)?
        };
        self.aircraft = update.state;
        update.context.write_into(&mut self.context_bytes)?;
        self.context = MissionContext::from_bytes(&self.context_bytes)?;
        let post = apply_callback_result(self.state, self.context, update.callback_result)?;
        self.state = post.state;
        self.context = post.context;
        self.context.write_into(&mut self.context_bytes)?;
        let mut sounds = Vec::new();
        let mut unhandled = Vec::new();
        let mut stream = None;
        let mut background_fade = false;
        let mut uploads = 0;
        let mut statistics = Vec::new();
        for effect in &update.effects {
            match effect {
                Effect::PendingOriginalCall {
                    address: 0x800d8cb4,
                } => {
                    let draw = self.aircraft_renderer.commands(
                        &self.scene.scene,
                        update
                            .draw_state
                            .as_ref()
                            .ok_or("aircraft draw has no boundary state")?,
                    )?;
                    self.scene.visual.apply_mission_visuals(&draw.effects)?;
                    statistics.extend(draw.packets);
                }
                Effect::LoadAircraftResources { index, rectangle } => {
                    uploads += self
                        .aircraft_renderer
                        .load_resources(*index, *rectangle, &mut self.scene.scene.vram)?
                        .len();
                }
                Effect::PlaySound { id } => sounds.push(*id),
                Effect::StartStream { resource } => {
                    stream = Some(json!({"action":"start","resource":resource}))
                }
                Effect::StopStream => stream = Some(json!({"action":"stop"})),
                Effect::PendingOriginalCallWithArgs {
                    address: 0x800e7a18,
                    arguments,
                } if arguments == &[30, 10] => background_fade = true,
                Effect::SelectVisualPage { .. }
                | Effect::ClearTextVisibility
                | Effect::SetTextVisibility { .. }
                | Effect::SetVisualStyle { .. }
                | Effect::SetVisualOffset { .. }
                | Effect::SetVisualParameter { .. } => {
                    self.scene
                        .visual
                        .apply_mission_visuals(std::slice::from_ref(effect))?;
                }
                _ => unhandled.push(format!("{effect:?}")),
            }
        }
        unhandled.extend(post.effects.iter().map(|e| format!("{e:?}")));
        self.scene.visual.set_frame_color(self.scene.color.rgb);
        let mut packets = self
            .scene
            .scene
            .mission_commands(&mut self.scene.visual, self.language)?
            .into_packets();
        packets.extend(statistics.into_iter().rev());
        let depth = self.scene.map.tick(self.scene.color.rgb)?;
        let depth: Vec<_> = depth
            .execution_order()
            .iter()
            .map(|p| p.words.clone())
            .collect();
        let image = self.scene.render(
            &HubLayers {
                packets,
                clear_color,
                remaining_effects: Vec::new(),
            },
            &depth,
        )?;
        if post.callback_result != 0 {
            self.pending = Some("mission_resident_transition_pending");
        } else if ![0, 1, 2, 3, 4].contains(&self.state.state) {
            self.pending = Some("mission_callback_pending");
        }
        let trace = json!({"frame":frame,"input":input,"mission_state_before":before.state,"mission_substate_before":before.substate,
            "mission_state":self.state.state,"mission_substate":self.state.substate,"result":update.callback_result,"resident_result":post.callback_result,
            "aircraft_phase":self.aircraft.phase,"aircraft_substate":self.aircraft.substate,"aircraft_selection":self.aircraft.selection,
            "aircraft_id":update.context.aircraft_id,"weapon_row":self.aircraft.weapon_row,"weapon_choices":self.aircraft.weapon_choice,
            "map_frame":self.scene.map.state.frame,"texture_uploads":uploads,"sound_requests":sounds,"stream_request":stream,
            "begin_briefing_background_fade":background_fade,"unhandled_effects":unhandled});
        Ok((image, trace))
    }
}
