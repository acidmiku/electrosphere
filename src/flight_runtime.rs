//! Native resident state-2 loading. Local reads complete on the next poll;
//! each preparation callback completes its ordered work in one host update.
//! Gameplay initialization remains an explicit boundary, not a placeholder scene.
use crate::{
    Result,
    assets::GameData,
    flight_assets::{
        FlightAircraftAssets, FlightMissionAssets, ResidentSection, Upload, UploadSource,
    },
    flight_loader::{
        FlightLoadObservation, FlightLoader, FlightLoadingContext, FlightLoadingEffect,
        FlightLoadingState, FlightLoadingUpdate, LoadingRenderStatus,
    },
    flight_resource_loader::{
        PreparationCommand, PreparationKind, PreparationPlan, SectionAddress, aircraft_preparation,
        mission_preparation,
    },
    menu_scene::MenuFrame,
    mission_map::Random,
    mission_runtime::MissionRuntime,
    sha256,
};
use ac3_audio::{Bank, SPU_RAM_BYTES, Sound};
use ac3_formats::dot1;
use ac3_graphics::{Frame, Vram, render_packets};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Sample bytes and descriptors installed by 2D544. No voice engine or reverb
/// memory is inferred from this storage; later playback consumes these bindings.
pub struct FlightSoundStorage {
    pub samples: Vec<u8>,
    pub descriptors: BTreeMap<u16, Sound>,
}
impl FlightSoundStorage {
    fn inherited(data: &GameData) -> Result<Self> {
        let mut storage = Self {
            samples: vec![0; SPU_RAM_BYTES],
            descriptors: BTreeMap::new(),
        };
        for (resource, child) in [(0, 4), (69, 4), (72, 9)] {
            storage.install(Bank::parse(
                dot1::parse(data.resource(resource)?)?.child(child)?,
            )?)?;
        }
        Ok(storage)
    }
    fn install(&mut self, bank: Bank<'_>) -> Result<Value> {
        let start = bank.spu_address() as usize;
        let bytes = bank.sample_bytes();
        self.samples
            .get_mut(start..start + bytes.len())
            .ok_or("sound upload outside storage")?
            .copy_from_slice(bytes);
        for id in bank.first_sound_id()..bank.first_sound_id() + bank.sound_count() {
            self.descriptors.insert(id, bank.sound(id)?);
        }
        Ok(
            json!({"address":start,"bytes":bytes.len(),"first_id":bank.first_sound_id(),
            "count":bank.sound_count(),"sample_sha256":sha256(bytes)}),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlightResourceBinding {
    pub kind: PreparationKind,
    pub address: SectionAddress,
}

pub struct FlightRuntime<'a> {
    data: &'a GameData,
    loader: FlightLoader<'a>,
    context_bytes: Vec<u8>,
    pub context: FlightLoadingContext,
    pub loading: FlightLoadingState,
    pub render: LoadingRenderStatus,
    pub random: Random,
    pub vram: Vram,
    pub substate: u32,
    pub pending: Option<&'static str>,
    pub mission_assets: Option<FlightMissionAssets>,
    pub aircraft_assets: Option<FlightAircraftAssets>,
    pub bindings: BTreeMap<u16, FlightResourceBinding>,
    pub sound: FlightSoundStorage,
    pub allocation_bytes: [usize; 2],
    pub arena_top: Option<usize>,
    queued_resource: Option<i16>,
}

enum PreparedAssets {
    Mission(FlightMissionAssets),
    Aircraft(FlightAircraftAssets),
}
impl PreparedAssets {
    fn sections(&self) -> &[ResidentSection] {
        match self {
            Self::Mission(a) => a.sections(),
            Self::Aircraft(a) => a.sections(),
        }
    }
    fn bank(&self) -> Result<Bank<'_>> {
        Ok(match self {
            Self::Mission(a) => a.sound_bank()?,
            Self::Aircraft(a) => a.sound_bank()?,
        })
    }
    fn section(&self, index: usize) -> Result<&ResidentSection> {
        self.sections()
            .get(index)
            .ok_or_else(|| "preparation section outside assets".into())
    }
}

impl<'a> FlightRuntime<'a> {
    pub fn from_mission(data: &'a GameData, mission: &MissionRuntime<'_>) -> Result<Self> {
        if mission.pending != Some("mission_resident_transition_pending") {
            return Err("resident flight loader requires the launch transition".into());
        }
        let mut context_bytes = mission.game_context_bytes().to_vec();
        // Menu callbacks did not address the three resident resource counts.
        context_bytes.resize(context_bytes.len().max(0xf8c), 0);
        let context = FlightLoadingContext::from_bytes(&context_bytes)?;
        Ok(Self {
            data,
            loader: FlightLoader::new(&data.executable.payload)?,
            context_bytes,
            context,
            loading: FlightLoadingState::default(),
            render: LoadingRenderStatus::default(),
            random: mission.random,
            vram: mission.scene.scene.vram.clone(),
            substate: 0,
            pending: None,
            mission_assets: None,
            aircraft_assets: None,
            bindings: BTreeMap::new(),
            sound: FlightSoundStorage::inherited(data)?,
            allocation_bytes: [0; 2],
            arena_top: None,
            queued_resource: None,
        })
    }

    pub fn game_context_bytes(&self) -> &[u8] {
        &self.context_bytes
    }

    fn observation(&self, resource: Option<i16>, completed: bool) -> FlightLoadObservation {
        FlightLoadObservation {
            active_resource: resource.map_or(-1, i32::from),
            progress: if completed { 4096 } else { 0 },
            completed,
        }
    }

    pub fn step(&mut self, frame_number: u32) -> Result<(MenuFrame, Value)> {
        if self.pending.is_some() {
            return Err("resident loading advanced beyond its boundary".into());
        }
        self.context.frame_clock = frame_number;
        let before = self.substate;
        let mut preparation = Value::Null;
        let update = match before {
            0 => {
                let resource = self.loader.mission_resource(self.context.mission)?;
                let observation = self.observation(Some(resource), false);
                self.loader.initialize(
                    &mut self.loading,
                    &mut self.context,
                    &mut self.render,
                    observation,
                    &mut self.random,
                )?
            }
            1 | 4 => {
                let resource = self
                    .queued_resource
                    .ok_or("resident poll without queued data")?;
                // GameData owns the checked resource; this is actual host completion.
                self.data.resource(usize::try_from(resource)?)?;
                let observation = self.observation(Some(resource), true);
                self.loader.poll(
                    &mut self.loading,
                    &self.context,
                    &mut self.render,
                    observation,
                    &mut self.random,
                )?
            }
            2 | 5 => {
                let kind = if before == 2 {
                    PreparationKind::Mission
                } else {
                    PreparationKind::Aircraft
                };
                let (update, report) = self.prepare(kind)?;
                preparation = report;
                update
            }
            3 => {
                let resource = self.loader.aircraft_resource(self.context.aircraft)?;
                let observation = self.observation(Some(resource), false);
                self.loader.queue_aircraft(
                    &mut self.loading,
                    &self.context,
                    &mut self.render,
                    observation,
                    &mut self.random,
                )?
            }
            _ => return Err("resident flight callback is not connected".into()),
        };
        let mut reset_voices = false;
        let mut queue_events = Vec::new();
        for effect in &update.effects {
            match *effect {
                FlightLoadingEffect::ResetResourceQueue => self.queued_resource = None,
                FlightLoadingEffect::ResetSound => reset_voices = true,
                FlightLoadingEffect::QueueResource { resource } => {
                    let bytes = self.data.resource(usize::try_from(resource)?)?.len();
                    self.queued_resource = Some(resource);
                    queue_events.push(json!({"resource":resource,"bytes":bytes}));
                }
            }
        }
        if update.callback_result == 0x04000000 {
            self.substate += 1;
        } else if update.callback_result != 0 {
            return Err("unexpected resident loading callback result".into());
        }
        if self.substate == 6 {
            self.pending = Some("flight_gameplay_initialization_pending");
        }
        self.context.write_into(&mut self.context_bytes)?;
        let packets = update.commands.execution_order();
        let mut frame = Frame::new(320, 256)?;
        let [red, green, blue] = self.render.clear_rgb().map(|v| u16::from(v >> 3));
        frame.clear(red | (green << 5) | (blue << 10));
        render_packets(&mut frame, &self.vram, &packets)?;
        let trace = json!({"frame":frame_number,"resident_state":2,
            "resident_substate_before":before,"resident_substate":self.substate,
            "result":update.callback_result,"loading_frame":self.loading.frame,
            "loading_stage":self.loading.stage,"loading_progress":self.loading.progress,
            "loading_pending":self.loading.pending,"random_state":self.random.state,
            "mission_id":self.context.mission,"aircraft_id":self.context.aircraft,
            "queued_resources":queue_events,"preparation":preparation,
            "reset_resident_voices":reset_voices,"sound_requests":[],"unhandled_effects":[],
            "render_flags":self.render.flags,"packet_count":packets.len(),
            "frame_width":320,"frame_height":256});
        Ok((MenuFrame { frame, packets }, trace))
    }

    fn prepare(&mut self, kind: PreparationKind) -> Result<(FlightLoadingUpdate, Value)> {
        let resource = self
            .queued_resource
            .ok_or("resident preparation without a resource")?;
        let expected = match kind {
            PreparationKind::Mission => self.loader.mission_resource(self.context.mission)?,
            PreparationKind::Aircraft => self.loader.aircraft_resource(self.context.aircraft)?,
        };
        if resource != expected || self.loading.pending != 0 {
            return Err("resident preparation before the requested resource completed".into());
        }
        let bytes = self.data.resource(usize::try_from(resource)?)?;
        // Eager parsing validates owned sections; the plan controls visibility of
        // each upload/binding. The parser never reads captured state or pointers.
        let (assets, plan, uploads) = match kind {
            PreparationKind::Mission => {
                let mut assets = FlightMissionAssets::parse(bytes)?;
                let plan = mission_preparation(&assets)?;
                let graphics =
                    assets.prepare_graphics(&self.data.executable.payload, &self.vram)?;
                (PreparedAssets::Mission(assets), plan, graphics.uploads)
            }
            PreparationKind::Aircraft => {
                let assets = FlightAircraftAssets::parse(bytes)?;
                let plan = aircraft_preparation(&assets)?;
                let graphics = assets.prepare_graphics(&self.vram)?;
                (PreparedAssets::Aircraft(assets), plan, graphics.uploads)
            }
        };
        let (update, report) = self.execute_preparation(&assets, &plan, &uploads, resource)?;
        match assets {
            PreparedAssets::Mission(a) => self.mission_assets = Some(a),
            PreparedAssets::Aircraft(a) => self.aircraft_assets = Some(a),
        }
        self.queued_resource = None;
        Ok((update, report))
    }

    fn execute_preparation(
        &mut self,
        assets: &PreparedAssets,
        plan: &PreparationPlan,
        uploads: &[Upload],
        resource: i16,
    ) -> Result<(FlightLoadingUpdate, Value)> {
        use PreparationCommand as C;
        let allocation_index = usize::from(plan.kind == PreparationKind::Aircraft);
        let mut screen = None;
        let mut commands = Vec::new();
        let mut sound = Value::Null;
        let mut upload_count = 0;
        let mut gpu_syncs = 0;
        let mut upload_bytes = 0;
        for command in &plan.commands {
            commands.push(format!("{command:?}"));
            match *command {
                C::ResolveResourceDirectory | C::PreserveCompressedPrefix { .. } => {
                    // GameData and PreparedAssets retain immutable validated source;
                    // the original in-place expansion's temporary copy is unnecessary.
                }
                C::BindSoundBank { .. } => sound = self.sound.install(assets.bank()?)?,
                C::UploadTextureGroup { child } => {
                    let group = uploads.iter().filter(|u| match u.source {
                        UploadSource::TimPixels { section, .. }
                        | UploadSource::TimClut { section, .. } => section == child,
                        UploadSource::DerivedPalette { .. } => false,
                    });
                    for upload in group {
                        upload.apply(&mut self.vram)?;
                        upload_count += 1;
                        upload_bytes += upload.bytes.len();
                    }
                }
                C::SynchronizeGpu => {
                    // CPU rasterizer/load_block is synchronous before this point.
                    gpu_syncs += 1;
                }
                C::ExpandSection { section } => {
                    assets.section(section)?;
                }
                C::BindResource { offset, address } => {
                    if address.offset >= assets.section(address.section)?.bytes().len() {
                        return Err("resident binding leaves decoded section".into());
                    }
                    self.bindings.insert(
                        offset,
                        FlightResourceBinding {
                            kind: plan.kind,
                            address,
                        },
                    );
                }
                C::SetContextCount { offset, count } => {
                    let offset = usize::from(offset);
                    self.context_bytes
                        .get_mut(offset..offset + 4)
                        .ok_or("resident count outside context")?
                        .copy_from_slice(&count.to_le_bytes());
                }
                C::RelocateDirectory { section } => {
                    assets
                        .section(section)?
                        .directory
                        .as_ref()
                        .ok_or("resident directory missing")?;
                }
                C::RelocateRecords {
                    section,
                    fields,
                    logical_records,
                } => {
                    let directory = assets
                        .section(section)?
                        .directory
                        .as_ref()
                        .ok_or("resident records missing")?;
                    if directory.fields_per_record != fields
                        || directory.slots.len() != logical_records
                    {
                        return Err("resident record relocation mismatch".into());
                    }
                }
                C::ReplaceObjectSlots {
                    first,
                    source_section,
                    count,
                } => {
                    let PreparedAssets::Mission(mission) = assets else {
                        return Err("aircraft object-slot replacement".into());
                    };
                    let source = assets
                        .section(source_section)?
                        .directory
                        .as_ref()
                        .ok_or("replacement directory missing")?;
                    for index in 0..count {
                        let reference = mission
                            .object_slots()
                            .get(first + index)
                            .ok_or("replacement slot missing")?;
                        if reference.section != source_section
                            || reference.offset != source.slots[index]
                        {
                            return Err("resident object alias mismatch".into());
                        }
                    }
                }
                C::PrepareMissionPalettes => {
                    for upload in uploads
                        .iter()
                        .filter(|u| matches!(u.source, UploadSource::DerivedPalette { .. }))
                    {
                        upload.apply(&mut self.vram)?;
                        upload_count += 1;
                        upload_bytes += upload.bytes.len();
                    }
                }
                C::SetArenaEnd { allocation_bytes } => {
                    self.allocation_bytes[allocation_index] = allocation_bytes
                }
                C::RecordArenaTop {
                    allocation_bytes, ..
                } => {
                    if self.allocation_bytes[allocation_index] != allocation_bytes {
                        return Err("resident arena end mismatch".into());
                    }
                    self.arena_top = Some(self.allocation_bytes.iter().sum());
                }
                C::AdvanceLoadingScreen => {
                    if screen.is_some() {
                        return Err("duplicate preparation screen update".into());
                    }
                    let observation = self.observation(None, false);
                    screen = Some(self.loader.poll(
                        &mut self.loading,
                        &self.context,
                        &mut self.render,
                        observation,
                        &mut self.random,
                    )?);
                }
                C::ResetRenderStatus { flags_or } => {
                    self.render.phase = 0;
                    self.render.parameter = 0;
                    self.render.flags |= flags_or;
                }
            }
        }
        if upload_count != uploads.len() {
            return Err("preparation did not submit every upload".into());
        }
        let mut update = screen.ok_or("missing preparation screen update")?;
        update.callback_result = plan.callback_result;
        Ok((
            update,
            json!({"resource":resource,"callback":plan.kind.original_callback(),
            "commands":commands,"texture_uploads":upload_count,"upload_bytes":upload_bytes,
            "gpu_syncs":gpu_syncs,"sound_bank":sound,"allocation_bytes":self.allocation_bytes[allocation_index]}),
        ))
    }
}
