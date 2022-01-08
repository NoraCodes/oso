use std::{
    collections::HashMap,
    iter::{empty, once},
    sync::{Arc, RwLock, RwLockReadGuard},
};

use crate::{
    kb::KnowledgeBase,
    terms::{Call, Operation, Operator, Symbol, Term, ToPolarString, Value, Variable},
};

pub struct Query {
    pub variables: Vec<String>,
    pub(crate) term: Term,
    pub kb: Arc<RwLock<KnowledgeBase>>,
}

pub struct Bindings {
    variables: HashMap<String, Term>,
}

trait Goal {
    type Results: Iterator<Item = State>;
    fn run(self, state: State) -> Self::Results;
}

impl Query {
    pub fn run(self) -> impl Iterator<Item = HashMap<Symbol, Value>> {
        let Self {
            term,
            variables,
            kb,
        } = self;
        let state = State {
            kb,
            ..Default::default()
        };
        term.run(state).map(move |state| {
            variables
                .iter()
                .map(|v| {
                    (
                        Symbol(v.clone()),
                        state
                            .bindings
                            .get(v) // get binding
                            .map(|t| state.walk(t.clone())) // walk to deref
                            .map(|t| t.value().clone()) // convert to value
                            .unwrap_or_else(|| Value::Variable(Variable::new(v.clone()))), // default to an unbound variable (should be error?)
                    )
                })
                .collect()
        })
    }
}

impl Goal for Call {
    type Results = Box<dyn Iterator<Item = State>>;

    fn run(self, state: State) -> Self::Results {
        println!("run call: {}", self.to_polar());
        let kb = state.kb.clone();
        let rules = state
            .kb()
            .get_generic_rule(&self.name)
            .expect(&format!("no matching rules for {}", self.name))
            .get_applicable_rules(&self.args);
        Box::new(rules.into_iter().flat_map(move |r| {
            println!("matching: {}", r);
            // for each applicable rule
            // create a set of bindings for the input arguments
            // and construct the goals needed to evaluate the rule
            let bindings = HashMap::new();
            let mut inner_state = State {
                bindings,
                kb: kb.clone(),
            };

            let mut applicable = true;
            let mut variables = vec![];
            for (arg, param) in self.args.iter().zip(r.params.iter()) {
                let arg = (&state).walk(arg.clone());
                if let Value::Variable(v) = arg.value() {
                    variables.push(v.name.0.clone())
                }
                if !inner_state.unify(arg.clone(), param.parameter.clone()) {
                    applicable = false;
                    println!("Failed to unify: {} and {}", arg, param.parameter);
                    break;
                }
                if let Some(ref specializer) = param.specializer {
                    if !inner_state.isa(arg.clone(), specializer.clone()) {
                        println!("Failed to isa: {} and {}", arg, specializer);
                        applicable = false;
                        break;
                    }
                }
            }
            if applicable {
                let cloneable_state = state.clone();
                // run the body using the new frame (inner state)
                // then map the resultant state to recombine with the current frame (state)
                Box::new(r.body.clone().run(inner_state).map(move |inner_state| {
                    let mut new_state = cloneable_state.clone();
                    // TODO: could run this like query since we want to get a specific set of
                    // bindings out
                    // Also, check for any unresolved partials
                    for v in &variables {
                        new_state.bindings.insert(
                            v.clone(),
                            inner_state
                                .walk(inner_state.bindings.get(v).expect("must be bound").clone()),
                        );
                    }
                    new_state
                })) as Box<dyn Iterator<Item = State>>
            } else {
                Box::new(empty())
            }
        }))
    }
}

impl Goal for Term {
    type Results = Box<dyn Iterator<Item = State>>;
    fn run(self, state: State) -> Self::Results {
        println!("run term: {}", self.to_polar());
        use Value::*;
        match self.value() {
            Call(call) => {
               Box::new(call.clone().run(state))
            }
            Expression(op) => Box::new(op.clone().run(state)),
            Boolean(b) => if *b {
                Box::new(once(state))
            } else {
                Box::new(empty())
            },
            v => todo!("Implementing run for: {}", v.to_polar())
            // Number(_) => todo!(),
            // String(_) => todo!(),
            // ExternalInstance(_) => todo!(),
            // Dictionary(_) => todo!(),
            // Pattern(_) => todo!(),
            // List(_) => todo!(),
            // Variable(_) => todo!(),
            // RestVariable(_) => todo!(),
        }
    }
}

impl Operation {
    fn run(self, mut state: State) -> Box<dyn Iterator<Item = State>> {
        use crate::terms::Operator::*;
        println!("run operation: {}", self.to_polar());
        match self.operator {
            Unify | Eq => {
                if state.unify(self.args[0].clone(), self.args[1].clone()) {
                    Box::new(once(state))
                } else {
                    Box::new(empty())
                }
            }
            And => Box::new(self.args.into_iter().fold(
                Box::new(once(state)) as Box<dyn Iterator<Item = State>>,
                |states, term| Box::new(states.flat_map(move |state| term.clone().run(state))),
            )),
            o => todo!("implementing run for operation {}", o.to_polar()),
        }
    }
}

#[derive(Clone, Default)]
pub struct State {
    kb: Arc<RwLock<KnowledgeBase>>,
    pub bindings: HashMap<String, Term>,
}

/// A struct to represent a unify _goal_
///
/// The question: when do you use the goal versus calling unify directly?
/// There are two cases:
/// 1. You need to perform a unification after some other goal
/// 2. Unification might result in multiple new states
///
/// Currently (2) never happens. So always prefer to use the direct unification
/// for efficiency.
struct Unify {
    left: Term,
    right: Term,
}

impl Goal for Unify {
    type Results = std::vec::IntoIter<State>;

    fn run(self, mut state: State) -> Self::Results {
        if state.unify(self.left, self.right) {
            vec![state].into_iter()
        } else {
            vec![].into_iter()
        }
    }
}

impl State {
    fn walk(&self, term: Term) -> Term {
        println!(
            "Bindings: {{ {} }}",
            self.bindings
                .iter()
                .map(|(k, v)| format!("{} => {},", k, v))
                .collect::<Vec<String>>()
                .join("\n\t")
        );
        match term.value() {
            match_var!(var) => {
                match self.bindings.get(&var.0) {
                    Some(t) if t == &term => {
                        // var is unbound
                        t.clone()
                    }
                    Some(t) => {
                        let t = t.clone();
                        self.walk(t)
                    }
                    _ => term,
                }
            }
            _ => term,
        }
    }

    fn unify(&mut self, left: Term, right: Term) -> bool {
        println!("Unify: {} = {}", left, right);

        match (self.walk(left).value(), self.walk(right).value()) {
            (left, right) if left == right => {
                println!("Exactly equal");
                true
            }
            (match_var!(var), value) | (value, match_var!(var)) => {
                println!("Bind: {} = {}", var, value);
                self.bindings
                    .insert(var.0.clone(), Term::new_temporary(value.clone()));
                true
            }
            (l, r) => {
                println!("Unify failed: {} = {}", l, r);
                false
            }
        }
    }

    fn isa(&mut self, left: Term, right: Term) -> bool {
        use Value::*;
        let left = self.walk(left);
        match (left.value(), self.walk(right).value()) {
            (left, right) if left == right => true,
            // var isa Foo{...}
            (Variable(var), InstanceLiteral(lit)) => {
                if let Some(tag) = &var.type_info {
                    tag == &lit.tag.0
                } else {
                    let mut new_var = var.clone();
                    new_var.type_info = Some(lit.tag.0.clone());
                    self.bindings
                        .insert(var.name.0.clone(), left.clone_with_value(Variable(new_var)));
                    true
                }
                // TODO: isa fields too
            }
            _ => false,
        }
    }

    fn kb(&self) -> RwLockReadGuard<KnowledgeBase> {
        self.kb.read().unwrap()
    }
}
