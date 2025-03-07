use crate::model::ParsingContext;
use crate::pb::*;
use tract_hir::internal::*;
use tract_hir::ops;
use tract_hir::tract_core::ops::matmul::MatMulAxes;
use tract_hir::tract_core::ops::scan::ScanInfo;

pub fn lstm(
    _ctx: &ParsingContext,
    pb: &NodeProto,
) -> TractResult<(Box<dyn InferenceOp>, Vec<String>)> {
    let mut lstm = LSTM::default();

    let mut options = crate::model::optional_inputs(pb).skip(3);
    lstm.optional_bias_input = options.next().unwrap();
    lstm.optional_sequence_lens_input = options.next().unwrap();
    lstm.optional_initial_h_input = options.next().unwrap();
    lstm.optional_initial_c_input = options.next().unwrap();
    lstm.optional_p_input = options.next().unwrap();

    let mut options = crate::model::optional_outputs(pb);
    lstm.optional_y_output = options.next().unwrap();
    lstm.optional_y_h_output = options.next().unwrap();
    lstm.optional_y_c_output = options.next().unwrap();

    Ok((expand(lstm), vec![]))
}

#[derive(Debug, Clone, Hash)]
pub struct LSTM {
    pub optional_bias_input: Option<usize>,
    pub optional_sequence_lens_input: Option<usize>,
    pub optional_initial_h_input: Option<usize>,
    pub optional_initial_c_input: Option<usize>,
    pub optional_p_input: Option<usize>,
    pub optional_y_output: Option<usize>,
    pub optional_y_h_output: Option<usize>,
    pub optional_y_c_output: Option<usize>,
    pub f: Box<dyn TypedOp>,
    pub g: Box<dyn TypedOp>,
    pub h: Box<dyn TypedOp>,
}

impl_dyn_hash!(LSTM);

impl Default for LSTM {
    fn default() -> LSTM {
        LSTM {
            optional_bias_input: None,
            optional_sequence_lens_input: None,
            optional_initial_h_input: None,
            optional_initial_c_input: None,
            optional_p_input: None,
            optional_y_output: None,
            optional_y_h_output: None,
            optional_y_c_output: None,
            f: Box::new(ops::nn::sigmoid()),
            g: Box::new(ops::math::tanh()),
            h: Box::new(ops::math::tanh()),
        }
    }
}

impl Expansion for LSTM {
    fn name(&self) -> Cow<str> {
        "LSTM".into()
    }

    fn validation(&self) -> Validation {
        Validation::Rounding
    }

    fn rules<'r, 'p: 'r, 's: 'r>(
        &'s self,
        s: &mut Solver<'r>,
        inputs: &'p [TensorProxy],
        outputs: &'p [TensorProxy],
    ) -> TractResult<()> {
        let input_count = 3
            + self.optional_bias_input.is_some() as usize
            + self.optional_sequence_lens_input.is_some() as usize
            + self.optional_initial_h_input.is_some() as usize
            + self.optional_initial_c_input.is_some() as usize
            + self.optional_p_input.is_some() as usize;
        check_input_arity(inputs, input_count)?;
        let output_count = self.optional_y_output.is_some() as usize
            + self.optional_y_h_output.is_some() as usize
            + self.optional_y_c_output.is_some() as usize;
        check_output_arity(outputs, output_count)?;
        s.equals(&inputs[0].datum_type, &inputs[1].datum_type)?;
        s.equals(&inputs[0].datum_type, &inputs[2].datum_type)?;
        s.equals(&inputs[0].datum_type, &outputs[0].datum_type)?;
        s.equals(&inputs[0].rank, 3)?;
        s.equals(&inputs[1].rank, 3)?;
        s.equals(&inputs[2].rank, 3)?;
        s.equals(&inputs[1].shape[0], &inputs[2].shape[0])?; // num_directions
        s.equals(&inputs[1].shape[1], &inputs[2].shape[1])?; // 4*hidden_size
        s.equals(&inputs[2].shape[1], 4 * inputs[2].shape[2].bex())?; // hidden_size
        if let Some(b) = self.optional_bias_input {
            // bias
            s.equals(&inputs[b].datum_type, &inputs[0].datum_type)?;
            s.equals(&inputs[b].rank, 2)?;
            s.equals(&inputs[b].shape[0], &inputs[2].shape[0])?; // num_directions
            s.equals(&inputs[b].shape[1], 8 * inputs[2].shape[2].bex())?; // 8 * hidden_size
        }
        if let Some(seq_len) = self.optional_sequence_lens_input {
            s.equals(&inputs[seq_len].rank, 1)?;
            s.equals(&inputs[seq_len].shape[0], &inputs[0].shape[1])?; // batch_size
        }
        if let Some(initial_h) = self.optional_initial_h_input {
            s.equals(&inputs[initial_h].datum_type, &inputs[0].datum_type)?;
            s.equals(&inputs[initial_h].rank, 3)?;
            s.equals(&inputs[initial_h].shape[0], &inputs[1].shape[0])?; // num_directions
            s.equals(&inputs[initial_h].shape[1], &inputs[0].shape[1])?; // batch_size
            s.equals(&inputs[initial_h].shape[2], &inputs[2].shape[2])?; // hidden_size
        }
        if let Some(initial_c) = self.optional_initial_c_input {
            s.equals(&inputs[initial_c].datum_type, &inputs[0].datum_type)?;
            s.equals(&inputs[initial_c].rank, 3)?;
            s.equals(&inputs[initial_c].shape[0], &inputs[1].shape[0])?; // num_directions
            s.equals(&inputs[initial_c].shape[1], &inputs[0].shape[1])?; // batch_size
            s.equals(&inputs[initial_c].shape[2], &inputs[2].shape[2])?; // hidden_size
        }
        if let Some(p) = self.optional_p_input {
            s.equals(&inputs[p].datum_type, &inputs[0].datum_type)?;
            s.equals(&inputs[p].rank, 2)?;
            s.equals(&inputs[p].shape[0], &inputs[1].shape[0])?; // num_directions
            s.equals(&inputs[p].shape[1], 3 * inputs[2].shape[2].bex())?; // hidden_size
        }
        if let Some(y) = self.optional_y_output {
            s.equals(&outputs[y].rank, 4)?;
            s.equals(&outputs[y].shape[0], &inputs[0].shape[0])?; // seq_lentgh
            s.equals(&outputs[y].shape[1], &inputs[1].shape[0])?; // num_directions
            s.equals(&outputs[y].shape[2], &inputs[0].shape[1])?; // batch_size
            s.equals(&outputs[y].shape[3], &inputs[2].shape[2])?; // hidden_size
        }
        if let Some(y_h) = self.optional_y_h_output {
            s.equals(&outputs[y_h].datum_type, &inputs[0].datum_type)?;
            s.equals(&outputs[y_h].rank, 3)?;
            s.equals(&outputs[y_h].shape[0], &inputs[1].shape[0])?; // num_directions
            s.equals(&outputs[y_h].shape[1], &inputs[0].shape[1])?; // batch_size
            s.equals(&outputs[y_h].shape[2], &inputs[2].shape[2])?; // hidden_size
        }
        if let Some(y_c) = self.optional_y_c_output {
            s.equals(&outputs[y_c].datum_type, &inputs[0].datum_type)?;
            s.equals(&outputs[y_c].rank, 3)?;
            s.equals(&outputs[y_c].shape[0], &inputs[1].shape[0])?; // num_directions
            s.equals(&outputs[y_c].shape[1], &inputs[0].shape[1])?; // batch_size
            s.equals(&outputs[y_c].shape[2], &inputs[2].shape[2])?; // hidden_size
        }
        Ok(())
    }

    fn nboutputs(&self) -> TractResult<usize> {
        Ok(self.optional_y_output.is_some() as usize
            + self.optional_y_h_output.is_some() as usize
            + self.optional_y_c_output.is_some() as usize)
    }

    fn wire(
        &self,
        prefix: &str,
        target: &mut TypedModel,
        inputs: &[OutletId],
    ) -> TractResult<TVec<OutletId>> {
        use tract_hir::tract_core::ops::array::TypedConcat;
        let fore = self.wire_one_side(prefix, target, inputs, 0)?;
        let w_fact = target.outlet_fact(inputs[1])?;
        if w_fact.shape[0] == 2.into() {
            let back = self.wire_one_side(&format!("{}.back", prefix), target, inputs, 1)?;
            let mut outputs = tvec!(0.into(); self.nboutputs()?);
            if let Some(ix) = self.optional_y_output {
                outputs[ix] = target.wire_node(
                    format!("{}.merge_y_output", prefix),
                    TypedConcat::new(1),
                    &[fore[ix], back[ix]],
                )?[0];
            }
            if let Some(ix) = self.optional_y_h_output {
                outputs[ix] = target.wire_node(
                    format!("{}.merge_y_h_output", prefix),
                    TypedConcat::new(0),
                    &[fore[ix], back[ix]],
                )?[0];
            }
            if let Some(ix) = self.optional_y_c_output {
                outputs[ix] = target.wire_node(
                    format!("{}.merge_y_c_output", prefix),
                    TypedConcat::new(0),
                    &[fore[ix], back[ix]],
                )?[0];
            }
            Ok(outputs)
        } else {
            Ok(fore)
        }
    }
}

impl LSTM {
    #[allow(non_snake_case)]
    fn wire_one_side(
        &self,
        prefix: &str,
        target: &mut TypedModel,
        inputs: &[OutletId],
        dir: usize,
    ) -> TractResult<TVec<OutletId>> {
        use tract_hir::ops::{array, math, matmul, scan};

        let x_fact = target.outlet_fact(inputs[0])?.clone();
        let r_fact = target.outlet_fact(inputs[2])?.clone();

        let b_size = &x_fact.shape[1];
        let h_size = &r_fact.shape[2];

        let mut body = TypedModel::default();
        let mut outer_inputs = vec![];
        let mut input_mapping = vec![];

        macro_rules! target_wire {
            ($name: ident = $op: expr, $($param: expr),*) => {
                let $name = target.wire_node(
                    format!("{}.{}", prefix, stringify!($name)),
                    $op, [$($param),*].as_ref())?[0];
            }
        }

        macro_rules! wire {
            ($name: ident = $op: expr, $($param: expr),*) => {
                let $name = body.wire_node(
                    format!("{}.{}", prefix, stringify!($name)),
                    $op, [$($param),*].as_ref())?[0];
            }
        }

        let chunk = if dir == 0 { 1 } else { -1 };

        // X: onnx interface: [seq_length, batch_size, input_size]
        // move batch first
        target_wire!(x_batch_first = AxisOp::Move(1, 0), inputs[0]);
        // X: onnx interface: [batch_size, seq_length, input_size]
        // scan outer interface: idem
        // scann inner interface: [batch_size, chunk=1, input_size]
        // onnx inner interface: [batch_size, input_size]
        outer_inputs.push(x_batch_first);
        input_mapping.push(scan::InputMapping::Scan(ScanInfo { slot: 0, axis: 1, chunk }));
        let mut x_source_fact = target.outlet_fact(x_batch_first)?.without_value();
        x_source_fact.shape.set(1, 1.to_dim());
        let x_source = body.add_source("x_source", x_source_fact)?;
        wire!(Xt = AxisOp::Rm(1), x_source);

        // W: onnx interface: [num_directions, 4*hidden_size, input_size]
        // scan interfaces: [4*hidden_size, input_size]
        target_wire!(w_dir = array::Slice::new(0, dir, dir + 1), inputs[1]);
        target_wire!(w = AxisOp::Rm(0), w_dir);
        outer_inputs.push(w);
        input_mapping.push(scan::InputMapping::Full { slot: 1 });
        let W = body.add_source("w", target.outlet_fact(w)?.clone())?;

        // R: onnx interface: [num_directions, 4*hidden_size, hidden_size]
        // scan interfaces: [4*hidden_size, hidden_size]
        target_wire!(r_dir = array::Slice::new(0, dir, dir + 1), inputs[2]);
        target_wire!(r = AxisOp::Rm(0), r_dir);
        outer_inputs.push(r);
        input_mapping.push(scan::InputMapping::Full { slot: 2 });
        let R = body.add_source("r", target.outlet_fact(r)?.clone())?;

        // B: onnx interface: [num_directions, 8*hidden_size]
        let b = if let Some(slot) = self.optional_bias_input {
            target_wire!(b = array::Slice::new(0, dir, dir + 1), inputs[slot]);
            outer_inputs.push(b);
            input_mapping.push(scan::InputMapping::Full { slot });
            let b = body.add_source("b", target.outlet_fact(b)?.clone())?;
            Some(b)
        } else {
            None
        };

        if let Some(slot) = self.optional_sequence_lens_input {
            outer_inputs.push(inputs[slot]);
        }

        // initial h, optional: onnx: [num_directions, batch_size, hidden_size]
        // scan outer: [batch_size, chunk=1, hidden_size]
        // scan inner: [batch_size, chunk=1, hidden_size]
        // onnx inner: [batch_size, hidden_size]
        let initializer = if let Some(initial_h_input) = self.optional_initial_h_input {
            target_wire!(h_dir = array::Slice::new(0, dir, dir + 1), inputs[initial_h_input]);
            target_wire!(h = AxisOp::Rm(0), h_dir);
            target_wire!(h_chunk_ = AxisOp::Add(0), h);
            target_wire!(h_chunk = AxisOp::Move(1, 0), h_chunk_);
            outer_inputs.push(h_chunk);
            scan::StateInitializer::FromInput(initial_h_input)
        } else {
            scan::StateInitializer::Value(
                tensor0(0.0f32)
                    .broadcast_scalar_to_shape(&[
                        b_size.to_usize().unwrap(),
                        1,
                        h_size.to_usize().unwrap(),
                    ])?
                    .into_arc_tensor(),
            )
        };
        input_mapping.push(scan::InputMapping::State { initializer });
        let h_source = body.add_source(
            "h_source",
            x_fact.datum_type.fact(&[b_size.clone(), 1.to_dim(), h_size.clone()]),
        )?;

        let initializer = if let Some(initial_c_input) = self.optional_initial_c_input {
            target_wire!(c_dir = array::Slice::new(0, dir, dir + 1), inputs[initial_c_input]);
            target_wire!(c = AxisOp::Rm(0), c_dir);
            target_wire!(c_chunk_ = AxisOp::Add(0), c);
            target_wire!(c_chunk = AxisOp::Move(1, 0), c_chunk_);
            outer_inputs.push(c_chunk);
            scan::StateInitializer::FromInput(initial_c_input)
        } else {
            scan::StateInitializer::Value(
                tensor0(0.0f32)
                    .broadcast_scalar_to_shape(&[
                        b_size.to_usize().unwrap(),
                        1,
                        h_size.to_usize().unwrap(),
                    ])?
                    .into_arc_tensor(),
            )
        };
        input_mapping.push(scan::InputMapping::State { initializer });
        let c_source = body.add_source(
            "c_source",
            x_fact.datum_type.fact(&[b_size.clone(), 1.to_dim(), h_size.clone()]),
        )?;

        // P: onnx [num_directions, 3*hidde_size]
        let p = if let Some(slot) = self.optional_p_input {
            target_wire!(p = array::Slice::new(0, dir, dir + 1), inputs[slot]);
            outer_inputs.push(p);
            input_mapping.push(scan::InputMapping::Full { slot });
            let p = body.add_source("p", target.outlet_fact(p)?.clone())?;
            Some(p)
        } else {
            None
        };

        // drop sequence axis (chunk == 1)
        wire!(Ht_1 = AxisOp::Rm(1), h_source);
        // onnx inner: [batch_size, hidden_size]
        wire!(Ct_1 = AxisOp::Rm(1), c_source);
        // onnx inner: [batch_size, hidden_size]

        wire!(Wi = array::Slice::new(0, 0.to_dim() * h_size, 1.to_dim() * h_size), W);
        wire!(Wo = array::Slice::new(0, 1.to_dim() * h_size, 2.to_dim() * h_size), W);
        wire!(Wf = array::Slice::new(0, 2.to_dim() * h_size, 3.to_dim() * h_size), W);
        wire!(Wc = array::Slice::new(0, 3.to_dim() * h_size, 4.to_dim() * h_size), W);

        wire!(Ri = array::Slice::new(0, 0.to_dim() * h_size, 1.to_dim() * h_size), R);
        wire!(Ro = array::Slice::new(0, 1.to_dim() * h_size, 2.to_dim() * h_size), R);
        wire!(Rf = array::Slice::new(0, 2.to_dim() * h_size, 3.to_dim() * h_size), R);
        wire!(Rc = array::Slice::new(0, 3.to_dim() * h_size, 4.to_dim() * h_size), R);

        let biases = if let Some(b) = b {
            wire!(Wbi = array::Slice::new(1, 0.to_dim() * h_size, 1.to_dim() * h_size), b);
            wire!(Wbo = array::Slice::new(1, 1.to_dim() * h_size, 2.to_dim() * h_size), b);
            wire!(Wbf = array::Slice::new(1, 2.to_dim() * h_size, 3.to_dim() * h_size), b);
            wire!(Wbc = array::Slice::new(1, 3.to_dim() * h_size, 4.to_dim() * h_size), b);

            wire!(Rbi = array::Slice::new(1, 4.to_dim() * h_size, 5.to_dim() * h_size), b);
            wire!(Rbo = array::Slice::new(1, 5.to_dim() * h_size, 6.to_dim() * h_size), b);
            wire!(Rbf = array::Slice::new(1, 6.to_dim() * h_size, 7.to_dim() * h_size), b);
            wire!(Rbc = array::Slice::new(1, 7.to_dim() * h_size, 8.to_dim() * h_size), b);

            wire!(bi = math::add(), Wbi, Rbi);
            wire!(bo = math::add(), Wbo, Rbo);
            wire!(bf = math::add(), Wbf, Rbf);
            wire!(bc = math::add(), Wbc, Rbc);

            Some((bi, bo, bf, bc))
        } else {
            None
        };

        let peepholes = if let Some(p) = p {
            wire!(pi = array::Slice::new(1, 0.to_dim() * h_size, 1.to_dim() * h_size), p);
            wire!(po = array::Slice::new(1, 1.to_dim() * h_size, 2.to_dim() * h_size), p);
            wire!(pf = array::Slice::new(1, 2.to_dim() * h_size, 3.to_dim() * h_size), p);
            Some((pi, po, pf))
        } else {
            None
        };

        let matmul_t = matmul::MatMul { axes: MatMulAxes::default().transposing_b() };

        // it = f(Xt*(Wi^T) + Ht-1*(Ri^T) + Pi (.) Ct-1 + Wbi + Rbi)
        wire!(Xt_WiT = matmul_t.clone(), Xt, Wi);
        wire!(Ht_1_RiT = matmul_t.clone(), Ht_1, Ri);
        wire!(it0 = math::add(), Xt_WiT, Ht_1_RiT);
        let mut it0 = it0;
        if let Some(biases) = biases {
            wire!(it_bias = math::add(), it0, biases.0);
            it0 = it_bias;
        };
        if let Some(peephole) = peepholes {
            wire!(Pi_Ct_1 = math::mul(), peephole.0, Ct_1);
            wire!(it_peep = math::add(), Pi_Ct_1, it0);
            it0 = it_peep;
        }
        wire!(it = self.f.clone(), it0);

        // ft = f(Xt*(Wf^T) + Ht-1*(Rf^T) + Pf (.) Ct-1 + Wbf + Rbf)
        wire!(Xt_WfT = matmul_t.clone(), Xt, Wf);
        wire!(Ht_1_RfT = matmul_t.clone(), Ht_1, Rf);
        wire!(ft0 = math::add(), Xt_WfT, Ht_1_RfT);
        let mut ft0 = ft0;
        if let Some(biases) = biases {
            wire!(ft_bias = math::add(), ft0, biases.2);
            ft0 = ft_bias;
        };
        if let Some(peephole) = peepholes {
            wire!(Pf_Ct_1 = math::mul(), peephole.2, Ct_1);
            wire!(ft_peep = math::add(), Pf_Ct_1, ft0);
            ft0 = ft_peep;
        }
        wire!(ft = self.f.clone(), ft0);

        // ct = g(Xt*(Wc^T) + Ht-1*(Rc^T) + Wbc + Rbc)
        wire!(Xt_WcT = matmul_t.clone(), Xt, Wc);
        wire!(Ht_1_RcT = matmul_t.clone(), Ht_1, Rc);
        wire!(ct0 = math::add(), Xt_WcT, Ht_1_RcT);
        let mut ct0 = ct0;
        if let Some(biases) = biases {
            wire!(ct_bias = math::add(), ct0, biases.3);
            ct0 = ct_bias
        };
        wire!(ct = self.g.clone(), ct0);

        // Ct = ft (.) Ct-1 + it (.) ct
        wire!(ft_Ct_1 = math::mul(), ft, Ct_1);
        wire!(it_ct = math::mul(), it, ct);
        wire!(Ct = math::add(), ft_Ct_1, it_ct);

        // ot = f(Xt*(Wo^T) + Ht-1*(Ro^T) + Po (.) Ct + Wbo + Rbo)
        wire!(Xt_WoT = matmul_t.clone(), Xt, Wo);
        wire!(Ht_1_RoT = matmul_t, Ht_1, Ro);
        wire!(ot0 = math::add(), Xt_WoT, Ht_1_RoT);
        let mut ot0 = ot0;
        if let Some(biases) = biases {
            wire!(ot_bias = math::add(), ot0, biases.1);
            ot0 = ot_bias
        };
        if let Some(peephole) = peepholes {
            wire!(Po_Ct = math::mul(), peephole.1, Ct);
            wire!(ot_peep = math::add(), Po_Ct, ot0);
            ot0 = ot_peep;
        }
        wire!(ot = self.f.clone(), ot0);

        // Ht = ot (.) h(Ct)
        wire!(h_Ct = self.h.clone(), Ct);
        wire!(Ht = math::mul(), ot, h_Ct);

        // onnx inner interface: [batch_size, input_size]
        // add sequence axis (chunk == 1)
        wire!(Ht_fixed = AxisOp::Add(1), Ht);
        wire!(Ct_fixed = AxisOp::Add(1), Ct);
        body.set_output_outlets(&[Ht_fixed, Ct_fixed])?;

        let h_mapping = scan::OutputMapping {
            state: true,
            full_dim_hint: None,
            last_value_slot: self.optional_y_h_output,
            scan: self.optional_y_output.map(|slot| ScanInfo { slot, axis: 1, chunk }),
        };
        let c_mapping = scan::OutputMapping {
            state: true,
            full_dim_hint: None,
            last_value_slot: self.optional_y_c_output,
            scan: None,
        };

        let scan_outputs = target.wire_node(
            prefix,
            scan::Scan::new(
                body,
                input_mapping,
                vec![h_mapping, c_mapping],
                self.optional_sequence_lens_input,
                0,
            )?,
            &outer_inputs,
        )?;

        let mut result = tvec!();
        if let Some(slot) = self.optional_y_output {
            target_wire!(y_batch_middle = AxisOp::Move(1, 0), scan_outputs[slot]);
            target_wire!(y = AxisOp::Add(1), y_batch_middle);
            result.push(y);
        }
        if let Some(slot) = self.optional_y_h_output {
            target_wire!(y_h_batch_middle = AxisOp::Move(1, 0), scan_outputs[slot]);
            result.push(y_h_batch_middle);
        }
        if let Some(slot) = self.optional_y_c_output {
            target_wire!(y_c_batch_middle = AxisOp::Move(1, 0), scan_outputs[slot]);
            result.push(y_c_batch_middle);
        }

        Ok(result)
    }
}
