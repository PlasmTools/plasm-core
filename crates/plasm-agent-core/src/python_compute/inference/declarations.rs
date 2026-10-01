use super::*;

pub(super) struct Declarations {
    pub source: String,
    pub contracts: BTreeMap<String, Type>,
}
impl Default for Declarations {
    fn default() -> Self {
        Self {
            source: format!(
                "from typing import NewType, Never, Literal, overload\nimport datetime\n{}",
                crate::python_money::STUBS
            ),
            contracts: BTreeMap::from([("PlasmMoney".into(), Type::scalar(FieldType::Money))]),
        }
    }
}
impl Declarations {
    pub fn render(&mut self, value: &Type, depth: usize) -> Result<String, String> {
        if depth >= 64 {
            return Err("analysis declaration depth exceeds 64".into());
        }
        if value.nullable && value.shape != ValueShape::Null {
            let mut inner = value.clone();
            inner.nullable = false;
            return Ok(format!("{} | None", self.render(&inner, depth + 1)?));
        }
        if let Some((name, _)) = self
            .contracts
            .iter()
            .find(|(_, contract)| *contract == value)
        {
            return Ok(name.clone());
        }
        if let ValueShape::Record { fields } | ValueShape::ObservedRecord { fields, .. } =
            &value.shape
        {
            let name = format!("PlasmAnalysisType{}", self.contracts.len());
            self.contracts.insert(name.clone(), value.clone());
            let mut members = String::new();
            let mut indexed = Vec::new();
            for (field, contract) in fields {
                super::super::upstream::validate_member(field)?;
                let ty = self.render(contract, depth + 1)?;
                members.push_str(&format!("    {field}: {ty}\n"));
                indexed.push((field.clone(), ty));
            }
            members.push_str(&super::super::upstream::record_index_members(&indexed));
            self.source.push_str(&format!(
                "class {name}:\n{}",
                if members.is_empty() {
                    "    pass\n"
                } else {
                    &members
                }
            ));
            return Ok(name);
        }
        let base = match &value.shape {
            ValueShape::Null => "None".into(),
            ValueShape::Never => "Never".into(),
            ValueShape::Array { element } => format!("list[{}]", self.render(element, depth + 1)?),
            ValueShape::Union { variants } => variants
                .iter()
                .map(|v| self.render(v, depth + 1))
                .collect::<Result<Vec<_>, _>>()?
                .join(" | "),
            ValueShape::Temporal { kind, .. } => format!("datetime.{}", kind.python_name()),
            ValueShape::Scalar { field_type } => match field_type {
                FieldType::Boolean => "bool",
                FieldType::Integer => "int",
                FieldType::Number => "float",
                FieldType::MultiSelect => "list[str]",
                FieldType::Money => "PlasmMoney",
                FieldType::EntityRef { .. } | FieldType::Json | FieldType::Blob => "object",
                FieldType::Date | FieldType::Array => {
                    return Err("analysis requires an explicit temporal/array shape".into())
                }
                _ => "str",
            }
            .into(),
            ValueShape::Record { .. } | ValueShape::ObservedRecord { .. } => unreachable!(),
        };
        let plain = value.domain.is_none()
            && match &value.shape {
                ValueShape::Scalar { field_type } => matches!(
                    field_type,
                    FieldType::Boolean | FieldType::Integer | FieldType::Number | FieldType::String
                ),
                ValueShape::Temporal { wire, .. } => wire.is_none(),
                _ => true,
            };
        if plain {
            return Ok(base);
        }
        let name = format!("PlasmAnalysisType{}", self.contracts.len());
        self.contracts.insert(name.clone(), value.clone());
        self.source
            .push_str(&format!("{name} = NewType('{name}', {base})\n"));
        Ok(name)
    }
}
