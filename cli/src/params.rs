use reqwest::Url;
use scan_fmt::scan_fmt;
use std::io::Read;
use std::path::PathBuf;
use std::str::FromStr;
use tract_core::ops::konst::Const;
#[allow(unused_imports)]
use tract_itertools::Itertools;
use tract_libcli::profile::BenchLimits;

use tract_core::internal::*;
use tract_core::model::TypedModel;
use tract_hir::internal::*;
#[cfg(feature = "pulse")]
use tract_pulse::internal::*;
#[cfg(feature = "tf")]
use tract_tensorflow::tfpb::tensorflow::GraphDef;

use tract_nnef::ast::dump::Dumper;

use crate::TractResult;
use tract_libcli::display_params;
use tract_libcli::display_params::DisplayParams;
use tract_libcli::model::Model;
use tract_libcli::tensor;
use tract_libcli::tensor::{TensorValues, TensorsValues};

use readings_probe::*;

use super::info_usage;

use std::convert::*;

#[derive(Debug)]
enum ModelLocation {
    Fs(PathBuf),
    Http(Url),
}

impl ModelLocation {
    fn path(&self) -> Cow<std::path::Path> {
        match self {
            ModelLocation::Fs(p) => p.into(),
            ModelLocation::Http(u) => std::path::Path::new(u.path()).into(),
        }
    }

    fn is_dir(&self) -> bool {
        if let &ModelLocation::Fs(p) = &self {
            p.is_dir()
        } else {
            false
        }
    }

    fn read(&self) -> TractResult<Box<dyn Read>> {
        match self {
            ModelLocation::Fs(p) => Ok(Box::new(std::fs::File::open(p)?)),
            ModelLocation::Http(u) => Ok(Box::new(reqwest::blocking::get(u.clone())?)),
        }
    }
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum SomeGraphDef {
    NoGraphDef,
    #[cfg(feature = "kaldi")]
    Kaldi(tract_kaldi::KaldiProtoModel),
    Nnef(tract_nnef::ProtoModel),
    #[cfg(feature = "onnx")]
    Onnx(tract_onnx::pb::ModelProto, tract_onnx::model::ParseResult),
    #[cfg(feature = "tf")]
    Tf(GraphDef),
}

#[derive(Debug)]
pub struct ModelBuildingError(pub Box<dyn Model>, pub Box<dyn std::error::Error + Send + Sync>);

impl std::fmt::Display for ModelBuildingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&*self.1, f)
    }
}

impl std::error::Error for ModelBuildingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.1)
    }
}

#[cfg(not(feature = "pulse"))]
type PulsedModel = ();

/// Structure holding the parsed parameters.
#[derive(Clone)]
pub struct Parameters {
    pub graph: SomeGraphDef,

    pub pulsed_model: Option<Arc<PulsedModel>>,

    pub tract_model: Arc<dyn Model>,
    pub reference_model: Option<Arc<dyn Model>>,

    #[cfg(feature = "conform")]
    pub tf_model: Option<tract_tensorflow::conform::tf::Tensorflow>,

    #[cfg(not(feature = "conform"))]
    #[allow(dead_code)]
    pub tf_model: (),

    pub tensors_values: TensorsValues,
    pub assertions: Assertions,

    pub machine_friendly: bool,
    pub allow_random_input: bool,
    pub allow_float_casts: bool,
}

#[cfg(feature = "tf")]
type TfExt = tract_tensorflow::model::TfModelExtensions;
#[cfg(not(feature = "tf"))]
type TfExt = ();

impl Parameters {
    fn disco_model(matches: &clap::ArgMatches) -> TractResult<(ModelLocation, bool)> {
        let model = matches.value_of("model").context("Model argument required")?;
        let path = std::path::PathBuf::from(model);
        let (location, onnx_tc) = if model.starts_with("http://") || model.starts_with("https://") {
            (ModelLocation::Http(model.parse()?), false)
        } else if !path.exists() {
            bail!("model not found: {:?}", path)
        } else if std::fs::metadata(&path)?.is_file()
            && path.file_name().unwrap().to_string_lossy() == "graph.nnef"
        {
            (ModelLocation::Fs(path.parent().unwrap().to_owned()), false)
        } else if std::fs::metadata(&path)?.is_dir() && path.join("graph.nnef").exists() {
            (ModelLocation::Fs(path), false)
        } else if std::fs::metadata(&path)?.is_dir() && path.join("model.onnx").exists() {
            (ModelLocation::Fs(path.join("model.onnx")), true)
        } else {
            (ModelLocation::Fs(path), false)
        };
        Ok((location, onnx_tc))
    }

    fn load_model(
        matches: &clap::ArgMatches,
        probe: Option<&Probe>,
        location: &ModelLocation,
        tensors_values: &TensorsValues,
        symbol_table: &SymbolTable,
    ) -> TractResult<(SomeGraphDef, Box<dyn Model>, Option<TfExt>)> {
        let need_graph =
            matches.is_present("proto") || matches.subcommand_name() == Some("compare-pbdir");

        let format = matches.value_of("format").unwrap_or(
            if location.path().extension().map(|s| s == "onnx").unwrap_or(false) {
                "onnx"
            } else if location.path().extension().map(|s| s == "raw" || s == "txt").unwrap_or(false)
            {
                "kaldi"
            } else if location.is_dir()
                || location.path().to_string_lossy().ends_with(".tar")
                || location.path().to_string_lossy().ends_with(".tar.gz")
                || location.path().extension().map(|s| s == "tgz").unwrap_or(false)
            {
                "nnef"
            } else {
                "tf"
            },
        );
        let triplet: (SomeGraphDef, Box<dyn Model>, Option<TfExt>) = match format {
            #[cfg(feature = "kaldi")]
            "kaldi" => {
                let kaldi = tract_kaldi::kaldi();
                info_usage("loaded framework (kaldi)", probe);
                let mut graph = kaldi.proto_model_for_read(&mut *location.read()?)?;
                info_usage("proto model loaded", probe);
                if let Some(i) = matches.value_of("kaldi-adjust-final-offset") {
                    graph.adjust_final_offset = i.parse()?;
                }
                let parsed = kaldi.model_for_proto_model_with_symbols(&graph, symbol_table)?;
                if need_graph {
                    (SomeGraphDef::Kaldi(graph), Box::new(parsed), Option::<TfExt>::None)
                } else {
                    (SomeGraphDef::NoGraphDef, Box::new(parsed), Option::<TfExt>::None)
                }
            }
            "nnef" => {
                let nnef = super::nnef(matches);
                let mut proto_model = if location.is_dir() {
                    if let ModelLocation::Fs(dir) = location {
                        nnef.proto_model_for_path(dir)?
                    } else {
                        unreachable!();
                    }
                } else if location
                    .path()
                    .extension()
                    .map(|e| e.to_string_lossy().ends_with("gz"))
                    .unwrap_or(false)
                {
                    nnef.proto_model_for_read(&mut flate2::read::GzDecoder::new(
                        &mut *location.read()?,
                    ))?
                } else {
                    nnef.proto_model_for_read(&mut *location.read()?)?
                };
                for (ix, name) in proto_model.doc.graph_def.parameters.iter().enumerate() {
                    #[allow(unused_imports)]
                    use tract_nnef::ast::{LValue, RValue};
                    if let Some(over) = tensors_values
                        .by_name(name)
                        .or_else(|| tensors_values.by_input_ix(ix))
                        .and_then(|tv| tv.fact.as_ref())
                    {
                        let assignment_id = proto_model
                            .doc
                            .graph_def
                            .body
                            .iter()
                            .position(|a| a.left == LValue::Identifier(name.clone()))
                            .context("Coulnt not find input assignement in nnef body")?;
                        let mut formatted = vec![];
                        let ass = &mut proto_model.doc.graph_def.body[assignment_id];
                        let inv = if let RValue::Invocation(inv) = &mut ass.right {
                            inv
                        } else {
                            unreachable!();
                        };
                        assert!(inv.id == "external" || inv.id == "tract_core_external", "invalid id: expected 'external' or 'tract_core_external' but found {:?}", inv.id);
                        assert!(
                            inv.arguments.len() <= 2,
                            "expected 1 argument but found {:?} for inv.arguments={:?}",
                            inv.arguments.len(),
                            inv.arguments
                        );
                        assert_eq!(inv.arguments[0].id.as_deref(), Some("shape"));
                        Dumper::new(&mut formatted).rvalue(&inv.arguments[0].rvalue)?;
                        let shape = over
                            .shape
                            .concretize()
                            .context("Can only use concrete shapes in override")?;
                        info!(
                            "Overriding model input shape named \"{}\". Replacing {} by {:?}.",
                            name,
                            String::from_utf8_lossy(&formatted),
                            &shape
                        );
                        inv.arguments[0].rvalue = tract_nnef::ser::tdims(&shape);
                    }
                }
                info_usage("proto model loaded", probe);
                let graph_def = if need_graph {
                    SomeGraphDef::Nnef(proto_model.clone())
                } else {
                    SomeGraphDef::NoGraphDef
                };
                (
                    graph_def,
                    Box::new(
                        nnef.translate(&proto_model, symbol_table)
                            .map_err(|(g, e)| ModelBuildingError(Box::new(g), e.into()))?,
                    ),
                    Option::<TfExt>::None,
                )
            }
            #[cfg(feature = "onnx")]
            "onnx" => {
                let mut onnx = tract_onnx::onnx();
                if matches.is_present("onnx-ignore-output-shapes") {
                    onnx = onnx.with_ignore_output_shapes(true);
                }
                if matches.is_present("onnx-ignore-output-types") {
                    onnx = onnx.with_ignore_output_types(true);
                }
                info_usage("loaded framework (onnx)", probe);
                let graph = onnx.proto_model_for_read(&mut *location.read()?)?;
                info_usage("proto model loaded", probe);
                let path = &location.path().clone();
                let mut parsed = onnx.parse_with_symbols(&graph, path.to_str(), symbol_table)?;

                if matches.is_present("determinize") {
                    tract_onnx::Onnx::determinize(&mut parsed.model)?;
                }

                if need_graph {
                    (
                        SomeGraphDef::Onnx(graph, parsed.clone()),
                        Box::new(parsed.model),
                        Option::<TfExt>::None,
                    )
                } else {
                    (SomeGraphDef::NoGraphDef, Box::new(parsed.model), Option::<TfExt>::None)
                }
            }
            #[cfg(feature = "tf")]
            "tf" => {
                let tf = tract_tensorflow::tensorflow();
                info_usage("loaded framework (tf)", probe);
                let mut graph = tf.proto_model_for_read(&mut *location.read()?)?;
                info_usage("proto model loaded", probe);
                if matches.is_present("determinize") {
                    tract_tensorflow::Tensorflow::determinize(&mut graph)?;
                }
                let mut model_and_ext = tf.parse_graph_with_symbols(&graph, symbol_table)?;
                model_and_ext.1.initializing_nodes = matches
                    .values_of("tf-initializer-output-node")
                    .map(|values| {
                        values
                            .map(|name| model_and_ext.0.node_id_by_name(name))
                            .collect::<TractResult<Vec<usize>>>()
                    })
                    .transpose()?
                    .unwrap_or_default();
                if need_graph {
                    (SomeGraphDef::Tf(graph), Box::new(model_and_ext.0), Some(model_and_ext.1))
                } else {
                    (SomeGraphDef::NoGraphDef, Box::new(model_and_ext.0), Some(model_and_ext.1))
                }
            }
            _ => bail!(
                "Format {} not supported. You may need to recompile tract with the right features.",
                format
            ),
        };
        Ok(triplet)
    }

    fn kaldi_downsample<F, O>(raw_model: &mut Graph<F, O>, period: isize) -> TractResult<()>
    where
        F: std::fmt::Debug + Clone + Hash + Fact,
        O: std::fmt::Debug + std::fmt::Display + AsRef<dyn Op> + AsMut<dyn Op> + Clone + Hash,
        Graph<F, O>: SpecialOps<F, O>,
        tract_core::ops::Downsample: Into<O>,
    {
        if period != 1 {
            let mut outputs = raw_model.output_outlets()?.to_vec();
            let output_name = raw_model.node(outputs[0].node).name.clone();
            raw_model.node_mut(outputs[0].node).name = format!("{}-old", output_name);
            let id = raw_model.wire_node(
                output_name,
                tract_core::ops::Downsample::new(0, period as _, 0),
                &outputs[0..1],
            )?[0];
            if let Some(label) = raw_model.outlet_label(outputs[0]).map(|s| s.to_string()) {
                raw_model.set_outlet_label(id, label)?;
            }
            outputs[0] = id;
            raw_model.set_output_outlets(&outputs)?;
        }
        Ok(())
    }

    fn kaldi_context<F, O>(
        raw_model: &mut Graph<F, O>,
        left: usize,
        right: usize,
    ) -> TractResult<()>
    where
        F: std::fmt::Debug + Clone + Hash + Fact,
        O: std::fmt::Debug + std::fmt::Display + AsRef<dyn Op> + AsMut<dyn Op> + Clone + Hash,
        Graph<F, O>: SpecialOps<F, O>,
        tract_hir::ops::array::Pad: Into<O>,
    {
        let op = tract_core::ops::array::Pad::new(
            vec![(left, right), (0, 0)],
            tract_core::ops::array::PadMode::Edge,
        );
        let mut patch = ModelPatch::default();
        for input in raw_model.input_outlets()? {
            let tap = patch.tap_model(raw_model, *input)?;
            let pad = patch.wire_node(
                format!("{}-pad", raw_model.node(input.node).name),
                op.clone(),
                &[tap],
            )?[0];
            patch.shunt_outside(raw_model, *input, pad)?;
        }
        patch.apply(raw_model)?;
        Ok(())
    }

    fn use_onnx_test_case_data_set(
        symbol_table: &SymbolTable,
        inputs_dir: &std::path::Path,
    ) -> TractResult<Vec<TensorValues>> {
        let mut result = vec![];
        for file in inputs_dir.read_dir()? {
            let file = file?;
            let filename = file
                .file_name()
                .into_string()
                .map_err(|s| format_err!("Can't convert OSString to String ({:?})", s))?;
            let is_input = filename.starts_with("input_");
            let is_output = filename.starts_with("output_");
            if is_input || is_output {
                let ix = filename
                    .split('_')
                    .nth(1)
                    .unwrap()
                    .split('.')
                    .next()
                    .unwrap()
                    .parse::<usize>()?;
                let (name, tensor) = tensor::for_data(symbol_table, file.path().to_str().unwrap())?;
                result.push(TensorValues {
                    input_index: Some(ix).filter(|_| is_input),
                    output_index: Some(ix).filter(|_| is_output),
                    name,
                    values: tensor.value.concretize().map(|t| vec![t.into_tensor().into()]),
                    fact: Some(tensor.without_value()),
                    random_range: None,
                })
            }
        }
        Ok(result)
    }

    pub fn parse_npz(
        input: &str,
        get_values: bool,
        get_facts: bool,
    ) -> TractResult<Vec<TensorValues>> {
        let mut npz = ndarray_npy::NpzReader::new(
            std::fs::File::open(input).with_context(|| format!("opening {:?}", input))?,
        )?;
        let vectors = npz
            .names()?
            .iter()
            .map(|n| {
                if let Ok((turn, name)) = scan_fmt::scan_fmt!(n, "turn_{d}/{}.npy", usize, String) {
                    Ok((name, turn, tensor::for_npz(&mut npz, n)?))
                } else {
                    let name = n.trim_end_matches(".npy").to_string();
                    Ok((name, 0, tensor::for_npz(&mut npz, n)?))
                }
            })
            .collect::<TractResult<Vec<_>>>()?;
        let mut result = vec![];
        for (name, vals) in vectors.into_iter().group_by(|triple| triple.0.clone()).into_iter() {
            let vals: Vec<_> = vals
                .into_iter()
                .sorted_by_key(|(_, turn, _)| *turn)
                .map(|(_, _, tensor)| tensor.into_tvalue())
                .collect();
            result.push(TensorValues {
                input_index: None,
                output_index: None,
                name: Some(name),
                fact: if get_facts {
                    Some(InferenceFact::from(&*vals[0]).without_value())
                } else {
                    None
                },
                values: if get_values { Some(vals) } else { None },
                random_range: None,
            })
        }
        Ok(result)
    }

    fn parse_tensors(
        matches: &clap::ArgMatches,
        location: &ModelLocation,
        onnx_tc: bool,
        symbol_table: &SymbolTable,
    ) -> TractResult<TensorsValues> {
        let mut result = TensorsValues::default();

        if let Some(inputs) = matches.values_of("input") {
            for (ix, v) in inputs.enumerate() {
                let (name, fact) = tensor::for_string(symbol_table, v)?;
                result.add(TensorValues {
                    input_index: Some(ix),
                    output_index: None,
                    name,
                    values: fact.value.concretize().map(|t| vec![t.into_tensor().into()]),
                    fact: Some(fact.without_value()),
                    random_range: None,
                });
            }
        }

        if let Some(bundle) = matches.values_of("input-bundle") {
            warn!("Argument --input-bundle is deprecated and may be removed in a future release. Use --input-facts-from-bundle and/or --input-from-bundle instead.");
            for input in bundle {
                for tv in Self::parse_npz(input, true, true)? {
                    result.add(tv);
                }
            }
        }

        if let Some(bundle) = matches.values_of("input-facts-from-bundle") {
            for input in bundle {
                for tv in Self::parse_npz(input, false, true)? {
                    result.add(tv);
                }
            }
        }

        if let Some((_, sub)) = matches.subcommand() {
            if let Some(values) = sub.values_of("assert-output") {
                for (ix, o) in values.enumerate() {
                    let (name, fact) = tensor::for_string(symbol_table, o)?;
                    info!(
                        "Output assertion #{}: (named: {}) {:?}",
                        ix,
                        name.as_deref().unwrap_or(""),
                        fact
                    );
                    result.add(TensorValues {
                        input_index: None,
                        output_index: Some(ix),
                        name,
                        values: fact.value.concretize().map(|t| vec![t.into_tensor().into()]),
                        fact: Some(fact.without_value()),
                        random_range: None,
                    });
                }
            }

            if let Some(bundles) = sub.values_of("assert-output-bundle") {
                for bundle in bundles {
                    for tv in Self::parse_npz(bundle, true, false)? {
                        result.add(tv);
                    }
                }
            }
        }

        if onnx_tc {
            let data_set_name = matches.value_of("onnx-test-data-set").unwrap_or("test_data_set_0");

            for tv in Self::use_onnx_test_case_data_set(
                symbol_table,
                location.path().parent().unwrap().join(data_set_name).as_path(),
            )? {
                result.add(tv)
            }
        }

        if let Some((_, sub)) = matches.subcommand() {
            if let Some(ranges) = sub.values_of("random-range") {
                for (ix, spec) in ranges.enumerate() {
                    let (name, from, to) = if let Ok((name, from, to)) =
                        scan_fmt!(spec, "{}={f}..{f}", String, f32, f32)
                    {
                        (Some(name), from, to)
                    } else if let Ok((from, to)) = scan_fmt!(spec, "{f}..{f}", f32, f32) {
                        (None, from, to)
                    } else {
                        bail!("Can't parse random-range parameter {}", spec)
                    };
                    let tv = if let Some(name) = name {
                        result.by_name_mut_with_default(&name)
                    } else {
                        result.by_input_ix_mut_with_default(ix)
                    };
                    tv.random_range = Some(from..to);
                }
            }
        }

        Ok(result)
    }

    #[allow(unused_variables)]
    #[allow(clippy::type_complexity)]
    fn pipeline(
        matches: &clap::ArgMatches,
        probe: Option<&readings_probe::Probe>,
        raw_model: Box<dyn Model>,
        tf_model_extensions: Option<TfExt>,
        reference_stage: Option<&str>,
    ) -> TractResult<(Arc<dyn Model>, Option<Arc<PulsedModel>>, Option<Arc<dyn Model>>)> {
        let keep_last = matches.is_present("verbose");
        let stop_at = matches.value_of("pass").unwrap_or(if matches.is_present("optimize") {
            "optimize"
        } else {
            "before-optimize"
        });

        let nnef_cycle = matches.is_present("nnef-cycle");

        info!("Will stop at {}", stop_at);

        if stop_at == "load" {
            return Ok((raw_model.into(), None, None));
        }

        let mut inference_model: Option<Arc<InferenceModel>> = None;
        let mut typed_model: Option<Arc<TypedModel>> = None;
        #[allow(unused_mut)]
        let mut pulsed_model: Option<Arc<PulsedModel>> = None;
        let mut reference_model: Option<Arc<dyn Model>> = None;

        if raw_model.is::<InferenceModel>() {
            inference_model = Some(raw_model.downcast::<InferenceModel>().unwrap().into());
        } else if raw_model.is::<TypedModel>() {
            typed_model = Some(raw_model.downcast::<TypedModel>().unwrap().into());
        }

        macro_rules! stage {
            ($name:expr, $from:ident -> $to:ident, $block:expr) => {
                if let Some(from) = $from.take() {
                    info!(concat!("Running '", $name, "'"));
                    let mut last_model: Option<Box<dyn Model>> =
                        if keep_last { Some(Box::new(from.as_ref().clone())) } else { None };
                    let block: &dyn Fn(_) -> TractResult<_> = &$block;
                    let owned_model =
                        Arc::try_unwrap(from).unwrap_or_else(|from| from.as_ref().clone());
                    match block(owned_model).context(concat!("Error at stage ", $name)) {
                        Ok(it) => {
                            $to = Some(Arc::new(it));
                        }
                        Err(e) => {
                            if let Some(last_model) = last_model.take() {
                                return Err(ModelBuildingError(last_model, e.into()))?;
                            } else {
                                return Err(e)?;
                            }
                        }
                    }
                    info_usage(concat!("after ", $name), probe);
                    if reference_stage.as_deref() == Some($name) {
                        reference_model = Some($to.as_ref().unwrap().clone());
                    }
                    if stop_at == $name {
                        return Ok((
                                $to.take().expect("returnable model"),
                                pulsed_model,
                                reference_model,
                                ));
                    }
                } else {
                    debug!("Skip stage {}", $name);
                    if stop_at == $name {
                        bail!("Stage {} is skipped, it can not be used as stop with these input format or parameters.", $name);
                    }
                }
            };
        }

        stage!("analyse", inference_model -> inference_model,
        |mut m:InferenceModel| -> TractResult<_> {
            let result = m.analyse(!matches.is_present("analyse-fail-fast"));
            match result {
                Ok(_) => Ok(m),
                Err(e) => Err(ModelBuildingError(Box::new(m), e.into()).into())
            }});
        if let Some(ext) = tf_model_extensions {
            #[cfg(feature = "tf")]
            stage!("tf-preproc", inference_model -> inference_model, |m:InferenceModel| ext.preproc(m));
        }
        stage!("incorporate", inference_model -> inference_model, |m:InferenceModel| m.incorporate());
        stage!("type", inference_model -> typed_model, |m:InferenceModel| m.into_typed());
        stage!("declutter", typed_model -> typed_model, |mut m:TypedModel| {
            if matches.is_present("label-wires") {
                for node in 0..m.nodes().len() {
                    if m.outlet_label(node.into()).is_none() {
                        m.set_outlet_label(node.into(), m.node(node).name.to_string())?;
                    }
                }
            }
            let mut dec = tract_core::optim::Optimizer::declutter();
            if let Some(steps) = matches.value_of("declutter-step") {
                dec = dec.stopping_at(steps.parse()?);
            }
            dec.optimize(&mut m)?;
            Ok(m)
        });
        #[cfg(feature = "pulse")]
        {
            if let Some(spec) = matches.value_of("pulse") {
                stage!("pulse", typed_model -> pulsed_model, |m:TypedModel| {
                    let (sym, pulse) = if let Ok((s,p)) = scan_fmt!(spec, "{}={}", String, String) {
                        (s, parse_tdim(&m.symbol_table, &p)?)
                    } else if let Ok(i) = parse_tdim(&m.symbol_table, spec) {
                        ("S".to_owned(), i)
                    } else {
                        bail!("Can not parse pulse specification {}", spec)
                    };
                    let sym = m.symbol_table.sym(&sym);
                    PulsedModel::new(&m, sym, &pulse)
                });
                stage!("pulse-to-type", pulsed_model -> typed_model, |m:PulsedModel| m.into_typed());
                stage!("pulse-declutter", typed_model -> typed_model, |m:TypedModel| m.into_decluttered());
            }
        }
        if matches.is_present("half-floats") {
            stage!("half-float", typed_model -> typed_model, |m:TypedModel| {
                use tract_core::model::translator::Translate;
                tract_core::half::HalfTranslator.translate_model(&m)
            });
        }
        if let Some(set) = matches.values_of("set") {
            let mut values = SymbolValues::default();
            for set in set {
                let (key, value) = set
                    .split_once('=')
                    .with_context(|| format!("--set must be in the X=value form, got {}", set))?;
                let value: i64 = value
                    .parse()
                    .with_context(|| format!("value expected to be an integer, got {}", value))?;
                let key = typed_model.as_ref().unwrap().get_or_intern_symbol(key);
                values.set(&key, value);
            }
            stage!("set", typed_model -> typed_model, |m: TypedModel| {
                m.concretize_dims(&values)
            });
            stage!("set-declutter", typed_model -> typed_model, TypedModel::into_decluttered);
        }
        if nnef_cycle {
            stage!("nnef-cycle", typed_model -> typed_model, |m:TypedModel| {
                let nnef = super::nnef(matches);
                let mut vec = vec!();
                nnef.write(&m, &mut vec).context("Serializing")?;
                info!("Dumped, now reloading...");
                nnef.model_for_read(&mut &*vec).context("Deserializing")
            });
            stage!("nnef-declutter", typed_model -> typed_model, |m:TypedModel| m.into_decluttered());
        }
        if let Some(sub) = matches.value_of("extract-decluttered-sub") {
            stage!("extract", typed_model -> typed_model, |m:TypedModel| {
                let node = m.node_id_by_name(sub)?;
                Ok(m.nested_models(node)[0].1.downcast_ref::<TypedModel>().unwrap().clone())
            });
        }
        stage!("before-optimize", typed_model -> typed_model, Ok);
        stage!("optimize", typed_model -> typed_model, |mut m:TypedModel| {
            let mut opt = tract_core::optim::Optimizer::codegen();
            if let Some(steps) = matches.value_of("optimize-step") {
                opt = opt.stopping_at(steps.parse()?);
            }
            opt.optimize(&mut m)?;
            Ok(m)
        });
        Ok((typed_model.clone().unwrap(), pulsed_model, reference_model))
    }

    #[allow(unused_variables)]
    #[allow(clippy::let_unit_value)]
    /// Parses the command-line arguments.
    pub fn from_clap(matches: &clap::ArgMatches, probe: Option<&Probe>) -> TractResult<Parameters> {
        let symbol_table = SymbolTable::default();
        let (filename, onnx_tc) = Self::disco_model(matches)?;
        let tensors_values = Self::parse_tensors(matches, &filename, onnx_tc, &symbol_table)?;
        let (mut graph, mut raw_model, tf_model_extensions) =
            Self::load_model(matches, probe, &filename, &tensors_values, &symbol_table)?;

        info!("Model {:?} loaded", filename);
        info_usage("model loaded", probe);

        let (need_tensorflow_model, need_reference_model) = match matches.subcommand() {
            Some(("compare", sm)) => {
                if let Some(with) = sm.value_of("stage") {
                    (false, Some(with))
                } else {
                    (true, None)
                }
            }
            _ => (false, None),
        };

        #[cfg(not(feature = "conform"))]
        let tf_model = ();
        #[cfg(feature = "conform")]
        let tf_model = if need_tensorflow_model {
            info!("Tensorflow version: {}", tract_tensorflow::conform::tf::version());
            if matches.is_present("determinize") {
                if let SomeGraphDef::Tf(ref graph) = graph {
                    let graph = graph.write_to_bytes().unwrap();
                    Some(tract_tensorflow::conform::tf::for_slice(&graph)?)
                } else {
                    unreachable!()
                }
            } else {
                Some(tract_tensorflow::conform::tf::for_path(&filename)?)
            }
        } else {
            None
        };

        let need_proto = matches.is_present("proto")
            || (matches.subcommand_matches("compare").map(|sc| sc.is_present("pbdir")))
                .unwrap_or(false);

        if !need_proto {
            graph = SomeGraphDef::NoGraphDef;
        }

        if let Some(inputs) = matches.values_of("input-node") {
            let inputs: Vec<&str> = inputs.collect();
            raw_model.set_input_names(&inputs)?;
        };

        if let Some(outputs) = matches.values_of("output-node") {
            let outputs: Vec<&str> = outputs.collect();
            raw_model.set_output_names(&outputs)?;
        };

        if let Some(override_facts) = matches.values_of("override-fact") {
            for fact in override_facts {
                let (name, fact) = tensor::for_string(&symbol_table, fact)?;
                let node = raw_model.node_id_by_name(&name.unwrap())?;
                if let Some(inf) = raw_model.downcast_mut::<InferenceModel>() {
                    inf.set_outlet_fact(OutletId::new(node, 0), fact)?;
                } else if let Some(typ) = raw_model.downcast_mut::<TypedModel>() {
                    typ.set_outlet_fact(OutletId::new(node, 0), (&fact).try_into()?)?;
                }
            }
        };

        if let Some(consts) = matches.values_of("constantize") {
            for konst in consts {
                if let Some(value) =
                    tensors_values.by_name(konst).and_then(|tv| tv.values.as_ref()).and_then(|v| v.get(0))
                {
                    let value = value.clone().into_arc_tensor();
                    let id = raw_model.node_id_by_name(konst)?;
                    info!("Commuting {} into a const of {:?}", raw_model.node_display(id), value);
                    let op = Box::new(Const::new(value.clone().into_arc_tensor()));
                    if let Some(inf) = raw_model.downcast_mut::<InferenceModel>() {
                        inf.inputs.retain(|i| i.node != id);
                        inf.nodes[id].op = op;
                    } else if let Some(typ) = raw_model.downcast_mut::<TypedModel>() {
                        typ.inputs.retain(|i| i.node != id);
                        typ.nodes[id].op = op;
                        typ.nodes[id].outputs[0].fact = TypedFact::from(value.clone());
                    }
                }
            }
        }

        let output_names_and_labels: Vec<Vec<String>> = raw_model
            .output_outlets()
            .iter()
            .map(|o| {
                let mut v = vec![format!("{}:{}", raw_model.node_name(o.node), o.slot)];
                if o.slot == 0 {
                    v.push(raw_model.node_name(o.node).to_string());
                }
                if let Some(l) = raw_model.outlet_label(*o) {
                    v.push(l.to_string());
                }
                v
            })
            .collect();

        let assertions = match matches.subcommand() {
            Some(("dump" | "run", sm)) => Assertions::from_clap(sm, &symbol_table)?,
            _ => Assertions::default(),
        };

        if let Some(sub) = matches.value_of("kaldi-downsample") {
            dispatch_model_mut_no_pulse!(raw_model, |m| Self::kaldi_downsample(m, sub.parse()?))?;
        }

        if matches.value_of("kaldi-left-context").is_some()
            || matches.value_of("kaldi-right-context").is_some()
        {
            let left = matches.value_of("kaldi-left-context").unwrap_or("0").parse()?;
            let right = matches.value_of("kaldi-right-context").unwrap_or("0").parse()?;
            dispatch_model_mut_no_pulse!(raw_model, |m| Self::kaldi_context(m, left, right))?;
        }

        if let Some(infer) = raw_model.downcast_mut::<InferenceModel>() {
            for (ix, node_id) in infer.inputs.iter().enumerate() {
                let tv = tensors_values
                    .by_name(&infer.node(node_id.node).name)
                    .or_else(|| tensors_values.by_input_ix(ix));
                if let Some(tv) = tv {
                    if let Some(fact) = &tv.fact {
                        infer.nodes[node_id.node].outputs[0].fact = fact.clone();
                    }
                }
            }
        }

        if matches.is_present("partial") {
            if let Some(m) = raw_model.downcast_ref::<InferenceModel>() {
                raw_model = Box::new(m.clone().into_compact()?);
            } else if let Some(m) = raw_model.downcast_ref::<TypedModel>() {
                raw_model = Box::new(m.clone().into_compact()?);
            }
        }

        let allow_random_input: bool = matches.is_present("allow-random-input");
        if allow_random_input {
            warn!("Argument --allow-random-input as global argument is deprecated and may be removed in a future release. Please move this argument to the right of the subcommand.");
        }
        let allow_float_casts = matches.is_present("allow-float-casts");
        if allow_float_casts {
            warn!("Argument --allow-float-casts as global argument is deprecated and may be removed in a future release. Please move this argument to the right of the subcommand.");
        }

        Self::pipeline(matches, probe, raw_model, tf_model_extensions, need_reference_model).map(
            |(tract_model, pulsed_model, reference_model)| {
                info!("Model ready");
                info_usage("model ready", probe);
                Parameters {
                    graph,
                    pulsed_model,
                    tract_model,
                    reference_model,
                    tf_model,
                    tensors_values,
                    assertions,
                    machine_friendly: matches.is_present("machine-friendly"),
                    allow_random_input,
                    allow_float_casts,
                }
            },
        )
    }
}

pub fn bench_limits_from_clap(matches: &clap::ArgMatches) -> TractResult<BenchLimits> {
    let max_iters =
        matches.value_of("max-iters").map(usize::from_str).transpose()?.unwrap_or(100_000);
    let max_time = matches
        .value_of("max-time")
        .map(u64::from_str)
        .transpose()?
        .map(std::time::Duration::from_millis)
        .unwrap_or(std::time::Duration::from_secs(5));
    Ok(BenchLimits { max_iters, max_time })
}

pub fn display_params_from_clap(
    root_matches: &clap::ArgMatches,
    matches: &clap::ArgMatches,
) -> TractResult<DisplayParams> {
    Ok(DisplayParams {
        konst: matches.is_present("const"),
        cost: matches.is_present("cost"),
        profile: matches.is_present("profile"),
        left_column_width: 0,
        invariants: matches.is_present("invariants"),
        quiet: matches.is_present("quiet"),
        natural_order: matches.is_present("natural-order"),
        debug_op: matches.is_present("debug-op"),
        node_ids: matches.values_of("node-id").map(|values| {
            values.map(|id| tvec!((id.parse::<usize>().unwrap(), "".to_string()))).collect()
        }),
        node_name: matches.value_of("node-name").map(String::from),
        op_name: matches.value_of("op-name").map(String::from),
        //        successors: matches.value_of("successors").map(|id| id.parse().unwrap()),
        expect_core: root_matches.value_of("pass").unwrap_or("declutter") == "declutter"
            && !root_matches.is_present("optimize"),
        outlet_labels: matches.is_present("outlet-labels"),
        io: if matches.is_present("io-long") {
            display_params::Io::Long
        } else if matches.is_present("io-none") {
            display_params::Io::None
        } else {
            display_params::Io::Short
        },
        info: matches.is_present("info"),
        json: matches.is_present("json"),
    })
}

#[derive(Debug, Default, Clone)]
pub struct Assertions {
    pub assert_outputs: bool,
    pub assert_output_facts: Option<Vec<InferenceFact>>,
    pub assert_op_count: Option<Vec<(String, usize)>>,
}

impl Assertions {
    fn from_clap(sub: &clap::ArgMatches, symbol_table: &SymbolTable) -> TractResult<Assertions> {
        let assert_outputs =
            sub.is_present("assert-output") || sub.is_present("assert-output-bundle");
        let assert_output_facts: Option<Vec<InferenceFact>> = sub
            .values_of("assert-output-fact")
            .map(|vs| vs.map(|v| tensor::for_string(symbol_table, v).unwrap().1).collect());
        let assert_op_count: Option<Vec<(String, usize)>> =
            sub.values_of("assert-op-count").and_then(|vs| {
                vs.chunks(2)
                    .into_iter()
                    .map(|mut args| Some((args.next()?.to_string(), args.next()?.parse().ok()?)))
                    .collect()
            });

        Ok(Assertions { assert_outputs, assert_output_facts, assert_op_count })
    }
}
