use std::{
    cell::RefCell,
    collections::{hash_map::Entry, HashMap, HashSet},
};
use js_sys::{JsString, Object, Reflect};
use log::*;
use screeps::{
    constants::{look,ErrorCode, Part, ResourceType},
    enums::{StructureObject},
    find, game,
    local::{ObjectId,Position,RoomCoordinate},
    objects::{Creep, Source, ConstructionSite, StructureController, StructureContainer, StructureExtension, StructureSpawn},
    structure::{StructureType},
    prelude::*,
};
use wasm_bindgen::prelude::*;

mod logging;

// this is one way to persist data between ticks within Rust's memory, as opposed to
// keeping state in memory on game objects - but will be lost on global resets!
thread_local! {
    static CREEP_TARGETS: RefCell<HashMap<String, CreepTarget>> = RefCell::new(HashMap::new());
}

static INIT_LOGGING: std::sync::Once = std::sync::Once::new();

// this enum will represent a creep's lock on a specific target object, storing a js reference
// to the object id so that we can grab a fresh reference to the object each successive tick,
// since screeps game objects become 'stale' and shouldn't be used beyond the tick they were fetched
#[derive(Clone)]
enum CreepTarget {
    Construct(Position),
    Pickup(Position),
    Repair(Position),
    Deposit(Position),
    Harvest(ObjectId<Source>),
    Upgrade(ObjectId<StructureController>),
    Withdraw(ObjectId<StructureContainer>),
}

// add wasm_bindgen to any function you would like to expose for call from js
// to use a reserved name as a function name, use `js_name`:
#[cfg(feature = "generate-pixel")]
#[wasm_bindgen(js_name = loop)]
pub fn game_loop() {
    INIT_LOGGING.call_once(|| {
        // show all output of Info level, adjust as needed
        logging::setup_logging(logging::Info);
    });

    debug!("loop starting! CPU: {}", game::cpu::get_used());

    // mutably borrow the creep_targets refcell, which is holding our creep target locks
    // in the wasm heap
    CREEP_TARGETS.with(|creep_targets_refcell| {
        let mut creep_targets = creep_targets_refcell.borrow_mut();
        debug!("running creeps");
        for creep in game::creeps().values() {
            run_creep(&creep, &mut creep_targets);
        }
        assign_new_targets(&mut creep_targets);
    });

    debug!("running towers");
    for tower in game::structures().values() {
        if let StructureObject::StructureTower(tower) = tower {
            let available_energy = tower.store().get_used_capacity(Some(ResourceType::Energy));
            if available_energy <= 100 {
                //continue;
            }

            // Find the closest hostile creep
            if let Some(target) = tower.pos().find_closest_by_range(find::HOSTILE_CREEPS) {
                // Attack if in range
                if tower.pos().in_range_to(target.pos(), 20) {
                    tower.attack(&target);
                    debug!("Tower attacking hostile creep at {:?}", target.pos());
                }
            } else {
                // First, try to heal damaged creeps
                if let Some(damaged_creep) = tower.pos().find_closest_by_range(find::MY_CREEPS)
                    .filter(|creep| creep.hits() < creep.hits_max())
                {
                    tower.heal(&damaged_creep);
                    debug!("Tower healing damaged creep at {:?}", damaged_creep.pos());
                } else {
                    // If no creeps need healing, prioritize repairing damaged structures
                    let structures = tower.pos().find_in_range(find::STRUCTURES, 20);
                    let structure = structures.iter().filter(|s| s.as_repairable().is_some() && s.as_structure().hits() < s.as_structure().hits_max()).min_by_key(|s| s.as_structure().hits());
                    let rampart = structures.iter().filter(|s| matches!(s, StructureObject::StructureRampart(_)) && s.as_structure().hits() < s.as_structure().hits_max()).min_by_key(|s| s.as_structure().hits());

                    if let Some(rampart) = rampart {
                        tower.repair(rampart.as_repairable().unwrap());
                        debug!("Tower repairing damaged rampart at {:?}", rampart.pos());
                    } else if let Some(structure) = structure {
                        tower.repair(structure.as_repairable().unwrap());
                        debug!("Tower repairing damaged structure at {:?}", structure.pos());
                    }
                }
            }
        }
    }

    debug!("running spawns");
    let mut additional = 0;
    for spawn in game::spawns().values() {
        debug!("running spawn {}", String::from(spawn.name()));

        let harvesters = CREEP_TARGETS.with(|targets| {
            targets.borrow().iter()
                .filter(|(name, target)| matches!(target, CreepTarget::Harvest(_)) && game::creeps().values().any(|c| c.name() == name.as_str()))
                .count()
        });
        let transporters = game::creeps().values()
            .filter(|creep| creep.body().iter().any(|body| matches!(body.part(), Part::Carry)))
            .count();
        let sources = spawn.room().unwrap().find(find::SOURCES_ACTIVE, None).len();
        let energy_available = spawn.room().unwrap().energy_available();
        let energy_capacity = spawn.room().unwrap().energy_capacity_available();
        let creep_count = game::creeps().values().count();
        let name_base = game::time();
        let name = format!("{}-{}", name_base, additional);

        if (energy_available == energy_capacity || harvesters == 0 || transporters == 0) && creep_count < 6 {
            if harvesters < sources {
                match energy_available {
                    300..=549 => {
                        let body = [Part::Move, Part::Move, Part::Work, Part::Work];
                        match spawn.spawn_creep(&body, &name) {
                            Ok(()) => additional += 1,
                            Err(e) => warn!("couldn't spawn: {:?}", e),
                        }
                    },
                    550..=749 => {
                        let body = [Part::Move, Part::Move, Part::Move, Part::Work, Part::Work, Part::Work, Part::Work];
                        match spawn.spawn_creep(&body, &name) {
                            Ok(()) => additional += 1,
                            Err(e) => warn!("couldn't spawn: {:?}", e),
                        }
                    },
                    750.. => {
                        let body = [Part::Move, Part::Move, Part::Move, Part::Move, Part::Move, Part::Work, Part::Work, Part::Work, Part::Work, Part::Work];
                        match spawn.spawn_creep(&body, &name) {
                            Ok(()) => additional += 1,
                            Err(e) => warn!("couldn't spawn: {:?}", e),
                        }
                    },
                    _ => {}
                }
            } else {
                match energy_available {
                    300..=549 => {
                        let body = [Part::Move, Part::Move, Part::Carry, Part::Carry, Part::Work];
                        match spawn.spawn_creep(&body, &name) {
                            Ok(()) => additional += 1,
                            Err(e) => warn!("couldn't spawn: {:?}", e),
                        }
                    },
                    550..=799 => {
                        let body = [Part::Move, Part::Move, Part::Move, Part::Carry, Part::Carry, Part::Carry, Part::Carry, Part::Work, Part::Work];
                        match spawn.spawn_creep(&body, &name) {
                            Ok(()) => additional += 1,
                            Err(e) => warn!("couldn't spawn: {:?}", e),
                        }
                    },
                    800.. => {
                        let body = [Part::Move, Part::Move, Part::Move, Part::Move, Part::Carry, Part::Carry, Part::Carry, Part::Carry, Part::Work, Part::Work, Part::Work, Part::Work];
                        match spawn.spawn_creep(&body, &name) {
                            Ok(()) => additional += 1,
                            Err(e) => warn!("couldn't spawn: {:?}", e),
                        }
                    },
                    _ => {}
                }
            }
        }
    }

    // this should be removed if you're using RawMemory/serde for persistence
    if game::time() % 10 == 0 {
        info!("running memory cleanup");
        let mut alive_creeps = HashSet::new();
        for creep_name in game::creeps().keys() {
            alive_creeps.insert(creep_name);
        }

        if let Ok(memory_creeps) = Reflect::get(&screeps::memory::ROOT, &JsString::from("creeps")) {
            let memory_creeps: Object = memory_creeps.unchecked_into();
            for creep_name_js in Object::keys(&memory_creeps).iter() {
                let creep_name = String::from(creep_name_js.dyn_ref::<JsString>().unwrap());

                if !alive_creeps.contains(&creep_name) {
                    info!("deleting memory for dead creep {}", creep_name);
                    let _ = Reflect::delete_property(&memory_creeps, &creep_name_js);
                }
            }
        }
    }

    if game::cpu::bucket() == 10000 {
        let _ = game::cpu::generate_pixel();
    }

    if !game::cpu::unlocked() {
        let _ = game::cpu::unlock();
    }

    info!("done! cpu: {}", game::cpu::get_used())
}

fn run_creep(creep: &Creep, creep_targets: &mut HashMap<String, CreepTarget>) {
    if creep.spawning() {
        return;
    }
    let name = creep.name();
    debug!("running creep {}", name);

    if let Some(creep_target) = creep_targets.get(&name) {
        match creep_target {
            CreepTarget::Upgrade(controller_id)
                if creep.store().get_used_capacity(Some(ResourceType::Energy)) > 0 =>
            {
                info!("{}: upgrading", name);
                if let Some(controller) = controller_id.resolve() {
                    creep
                        .upgrade_controller(&controller)
                        .unwrap_or_else(|e| match e {
                            ErrorCode::NotInRange => {
                                let _ = creep.move_to(&controller);
                            }
                            _ => {
                                warn!("couldn't upgrade: {:?}", e);
                                creep_targets.remove(&name);
                            }
                        });
                } else {
                    creep_targets.remove(&name);
                }
            }

            CreepTarget::Construct(position)
                if creep.store().get_used_capacity(Some(ResourceType::Energy)) > 0 =>
            {
                info!("{}: constructing", name);
                if creep.pos().is_near_to(*position) {
                    if let Ok(results) = position.look_for(look::CONSTRUCTION_SITES) {
                        if let Some(site) = results.first() {
                            creep.build(&site).unwrap_or_else(|e| {
                                creep_targets.remove(&name);
                            });
                        } else {
                            if let Ok(ramparts) = position.look_for(look::STRUCTURES) {
                                if let Some(rampart) = ramparts.iter().find(|s| matches!(s, StructureObject::StructureRampart(_))) {
                                    if let StructureObject::StructureRampart(rampart) = rampart {
                                        creep.repair(rampart).unwrap_or_else(|e| {
                                            creep_targets.remove(&name);
                                        });
                                    } else {
                                        creep_targets.remove(&name);
                                    }
                                } else {
                                    creep_targets.remove(&name);
                                }
                            } else {
                                creep_targets.remove(&name);
                            }
                        }
                    } else {
                        creep_targets.remove(&name);
                    }
                } else {
                    let _ = creep.move_to(*position);
                }
            }

            CreepTarget::Harvest(source_id) =>
            {
                info!("{}: harvesting", name);
                if let Some(source) = source_id.resolve() {
                    if creep.pos().is_near_to(source.pos()) {
                        let containers = source.pos().find_in_range(find::STRUCTURES, 1);
                        if let Some(container) = containers.iter().find(|&s| matches!(s, StructureObject::StructureContainer(_))) {
                            if creep.pos() != container.pos() {
                                let _ = creep.move_to(container.pos());
                            } else {
                                creep.harvest(&source).unwrap_or_else(|e| {
                                    creep_targets.remove(&name);
                                });
                            }
                        } else {
                            creep.harvest(&source).unwrap_or_else(|e| {
                                creep_targets.remove(&name);
                            });
                        }
                    } else {
                        let _ = creep.move_to(&source);
                    }
                } else {
                    creep_targets.remove(&name);
                }
            }

            CreepTarget::Withdraw(structure_id)
                if creep.store().get_free_capacity(Some(ResourceType::Energy)) > 0 =>
            {
                info!("{}: withdrawing", name);
                if let Some(structure) = structure_id.resolve() {
                    if creep.pos().is_near_to(structure.pos()) {
                        creep.withdraw(&structure, ResourceType::Energy, None).unwrap_or_else(|e| {
                            creep_targets.remove(&name);
                        });
                    } else {
                        let _ = creep.move_to(&structure);
                    }
                } else {
                    creep_targets.remove(&name);
                }
            }
            CreepTarget::Pickup(position)
                if creep.store().get_free_capacity(Some(ResourceType::Energy)) > 0 =>
            {
                info!("{}: picking", name);
                match position.look_for(look::ENERGY) {
                    Ok(resources) => {
                        if let Some(resource) = resources.first() {
                            if creep.pos().is_near_to(*position) {
                                creep.pickup(resource).unwrap_or_else(|e| {
                                    creep_targets.remove(&name);
                                });
                            } else {
                                let _ = creep.move_to(*position);
                            }
                        } else {
                            creep_targets.remove(&name);
                        }
                    },
                    Err(e) => {
                        creep_targets.remove(&name);
                    }
                }
            }
            CreepTarget::Deposit(position)
                if creep.store().get_used_capacity(Some(ResourceType::Energy)) > 0 =>
            {
                info!("{}: depositing", name);
                let targets = position.look_for(look::STRUCTURES).unwrap_or_else(|_| {
                    Vec::new()
                });
                let structure = targets.iter().find(|s| {
                    matches!(s, StructureObject::StructureExtension(_) | StructureObject::StructureSpawn(_) | StructureObject::StructureTower(_))
                });
                if let Some(structure) = structure {
                    if creep.pos().is_near_to(structure.pos()) {
                        if let Some(structure) = structure.as_transferable() {
                            creep.transfer(structure, ResourceType::Energy, None).unwrap_or_else(|e| {
                                creep_targets.remove(&name);
                            });
                        }
                    } else {
                        let _ = creep.move_to(*position);
                    }
                } else {
                    creep_targets.remove(&name);
                }
            }
            CreepTarget::Repair(position)
                if creep.store().get_used_capacity(Some(ResourceType::Energy)) > 0 =>
            {
                info!("{}: repairing", name);
                if creep.pos().is_near_to(*position) {
                    if let Ok(structures) = position.look_for(look::STRUCTURES) {
                        if let Some(structure) = structures.iter().find(|s| s.as_structure().hits() < s.as_structure().hits_max()) {
                            if let Some(repairable) = structure.as_repairable() {
                                creep.repair(repairable).unwrap_or_else(|e| {
                                    creep_targets.remove(&name);
                                });
                                return;
                            }
                        }
                    }
                    creep_targets.remove(&name);
                } else {
                    let _ = creep.move_to(*position);
                }
            }
            _ => {
                info!("{}: clearing", name);
                creep_targets.remove(&name);
            }
        }
    }
}

fn assign_new_targets(creep_targets: &mut HashMap<String, CreepTarget>) {
    'creeps: for creep in game::creeps().values() {
        let name = creep.name();
        if !creep_targets.contains_key(&name) {
            info!("{}: assigning", name);
            let room = creep.room().expect("couldn't resolve creep room");
            if creep.store().get_used_capacity(Some(ResourceType::Energy)) > 0 {
                // Assign the creep to fill energy
                let spawns = room.find(find::MY_STRUCTURES, None)
                    .into_iter()
                    .filter_map(|s| match s {
                        StructureObject::StructureSpawn(spawn) if spawn.store().get_free_capacity(Some(ResourceType::Energy)) > 0 => Some(spawn),
                        _ => None
                    })
                    .collect::<Vec<_>>();
                let extensions = room.find(find::MY_STRUCTURES, None)
                    .into_iter()
                    .filter_map(|s| match s {
                        StructureObject::StructureExtension(ext) if ext.store().get_free_capacity(Some(ResourceType::Energy)) > 0 => Some(ext),
                        _ => None
                    })
                    .collect::<Vec<_>>();
                let towers = room.find(find::MY_STRUCTURES, None)
                    .into_iter()
                    .filter_map(|s| match s {
                        StructureObject::StructureTower(tower) if tower.store().get_free_capacity(Some(ResourceType::Energy)) > 0 => Some(tower),
                        _ => None
                    })
                    .collect::<Vec<_>>();

                if let Some(target) = extensions.iter().min_by_key(|ext| ext.store().get_free_capacity(Some(ResourceType::Energy))) {
                    if !creep_targets.values().any(|target| matches!(target, CreepTarget::Deposit(_))) {
                        creep_targets.insert(name, CreepTarget::Deposit(target.pos()));
                        continue;
                    }
                }

                if let Some(target) = spawns.iter().min_by_key(|spawn| spawn.store().get_free_capacity(Some(ResourceType::Energy))) {
                    if !creep_targets.values().any(|target| matches!(target, CreepTarget::Deposit(_))) {
                        creep_targets.insert(name, CreepTarget::Deposit(target.pos()));
                        continue;
                    }
                }

                if let Some(target) = towers.iter().min_by_key(|tower| tower.store().get_free_capacity(Some(ResourceType::Energy))) {
                    if !creep_targets.values().any(|target| matches!(target, CreepTarget::Deposit(_))) {
                        creep_targets.insert(name, CreepTarget::Deposit(target.pos()));
                        continue;
                    }
                }

                // constructors
                let construction_sites = room.find(find::MY_CONSTRUCTION_SITES, None);
                let defensive_sites = construction_sites.iter().filter(|site| site.structure_type() == StructureType::Rampart || site.structure_type() == StructureType::Wall || site.structure_type() == StructureType::Tower);
                let extension_sites = construction_sites.iter().filter(|site| site.structure_type() == StructureType::Extension);
                let container_sites = construction_sites.iter().filter(|site| site.structure_type() == StructureType::Container);
                let other_sites = construction_sites.iter().filter(|site| ![StructureType::Extension, StructureType::Container, StructureType::Rampart, StructureType::Wall, StructureType::Tower].contains(&site.structure_type()));

                if let Some(site) = defensive_sites.min_by_key(|site| site.progress_total() - site.progress()) {
                    if !creep_targets.iter().any(|(name, target)| matches!(target, CreepTarget::Construct(_))) && game::creeps().values().any(|c| c.name() == name.as_str()) {
                        creep_targets.insert(name, CreepTarget::Construct(site.pos()));
                        continue;
                    }
                }

                if let Some(site) = container_sites.min_by_key(|site| site.progress_total() - site.progress()) {
                    if !creep_targets.values().any(|target| matches!(target, CreepTarget::Construct(_))) {
                        creep_targets.insert(name, CreepTarget::Construct(site.pos()));
                        continue;
                    }
                }

                if let Some(site) = extension_sites.min_by_key(|site| site.progress_total() - site.progress()) {
                    if !creep_targets.values().any(|target| matches!(target, CreepTarget::Construct(_))) {
                        creep_targets.insert(name, CreepTarget::Construct(site.pos()));
                        continue;
                    }
                }

                if let Some(site) = other_sites.min_by_key(|site| site.progress_total() - site.progress()) {
                    if !creep_targets.values().any(|target| matches!(target, CreepTarget::Construct(_))) {
                        creep_targets.insert(name, CreepTarget::Construct(site.pos()));
                        continue;
                    }
                }

                // repairers
                let mut repairable = room.find(find::STRUCTURES, None)
                    .into_iter()
                    .filter(|s| s.as_repairable().map_or(false, |r| r.hits() < r.hits_max() / 2))
                    .collect::<Vec<_>>();
                repairable.sort_by_key(|s| {
                    if s.as_structure().structure_type() == StructureType::Rampart {
                        (s.as_structure().hits(), 0)
                    } else {
                        (s.as_structure().hits(), 1)
                    }
                });
                if !creep_targets.iter().any(|(name, target)| matches!(target, CreepTarget::Repair(_)) && game::creeps().values().any(|c| c.name() == name.as_str())) {
                    if let Some(structure) = repairable.first() {
                        creep_targets.insert(name, CreepTarget::Repair(structure.pos()));
                        continue;
                    }
                }

                // upgraders
                for structure in room.find(find::STRUCTURES, None).iter() {
                    if let StructureObject::StructureController(controller) = structure {
                        creep_targets.insert(name, CreepTarget::Upgrade(controller.id()));
                        continue 'creeps;
                    }
                }
            } else {
                let has_carry = creep.body().iter().any(|body| matches!(body.part(), Part::Carry));
                let containers = room.find(find::STRUCTURES, None)
                    .into_iter()
                    .filter_map(|s| match s {
                        StructureObject::StructureContainer(container) if container.store().get_used_capacity(Some(ResourceType::Energy)) >= creep.store().get_capacity(Some(ResourceType::Energy)) => Some(container),
                        _ => None
                    })
                    .collect::<Vec<_>>();

                let dropped = room.find(find::DROPPED_RESOURCES, None)
                    .into_iter()
                    .filter(|resource| resource.resource_type() == ResourceType::Energy && resource.amount() >= creep.store().get_capacity(Some(ResourceType::Energy)))
                    .collect::<Vec<_>>();

                if has_carry {
                    if let Some(container) = containers.iter().max_by_key(|&container| container.store().get_used_capacity(Some(ResourceType::Energy))) {
                        creep_targets.insert(name, CreepTarget::Withdraw(container.id()));
                        return;
                    } else if let Some(energy) = dropped.iter().max_by_key(|&energy| energy.amount()) {
                        creep_targets.insert(name, CreepTarget::Pickup(energy.pos()));
                        return;
                    }
                } else {
                    let active_sources = room.find(find::SOURCES_ACTIVE, None);
                    let source = active_sources.iter().find(|&source| {
                        !creep_targets.iter().any(|(name, target)| {
                            matches!(target, CreepTarget::Harvest(id) if *id == source.id()) && game::creeps().values().any(|c| c.name() == name.as_str())
                        })
                    });

                    if let Some(source) = source {
                        creep_targets.insert(name, CreepTarget::Harvest(source.id()));
                        return;
                    } else {
                        creep.suicide();
                    }
                }

                if let Ok(structures) = creep.pos().look_for(look::STRUCTURES) {
                    if structures.iter().any(|s| matches!(s, StructureObject::StructureRoad(_))) {
                        let rx: std::ops::RangeInclusive<i32> = -1..=1;
                        'dx: for dx in rx {
                            let ry: std::ops::RangeInclusive<i32> = -1..=1;
                            for dy in ry {
                                if dx == 0 && dy == 0 {
                                    continue;
                                }
                                let new_pos = Position::new(RoomCoordinate::new(creep.pos().x().u8() + (dx as u8)).unwrap(), RoomCoordinate::new(creep.pos().y().u8() + (dy as u8)).unwrap(), creep.room().unwrap().name());
                                if let Ok(structures) = new_pos.look_for(look::STRUCTURES) {
                                    if !structures.iter().any(|s| matches!(s, StructureObject::StructureRoad(_))) {
                                        let _ = creep.move_to(new_pos);
                                        break 'dx;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
