# Enums e seleção segura

## Objetivo

Enums representam um valor que está em exatamente um entre um conjunto fechado de casos. Um caso
pode carregar dados. Essa forma permite modelar estados, ausência e falhas esperadas sem `null`,
exceções ou valores sentinela.

## Sintaxe aceita

```aster
public enum Message
{
    Quit,
    Move(int x, int y),
}

Message message = Message.Move(20, 22);
```

Enums genéricos são especializados com tipos concretos antes da geração de código:

```aster
public enum Option<T>
{
    Some(T value),
    None,
}
```

## Switch

```aster
switch (message)
{
    case Move(x, y):
        return x + y;
    case Quit:
        return 0;
}
```

O valor selecionado é avaliado uma vez. Não existe fallthrough e `break` continua exclusivo de
loops. Cada arm possui escopo próprio. Sem `default`, todos os casos devem aparecer; casos
duplicados, inexistentes ou com número incorreto de bindings são erros.

## Representação

Um enum concreto é um valor formado por uma tag interna e armazenamento alinhado para o maior
payload. A tag não é parte da API e não pode ser convertida para inteiro. Cópia e igualdade leem
somente o payload do caso ativo. Não existe boxing ou alocação no heap por padrão.

## Exemplos inválidos

```aster
switch (message)
{
    case Quit:
        return 0;
}
```

O switch não cobre `Move` e não possui `default`.

```aster
Message message = Message.Move(10);
```

`Move` exige dois argumentos `int`.

## Limites atuais

Pattern matching aninhado, guards, switch expression, discriminantes numéricos, flags, casts de
enum, métodos em enum e valores default implícitos não existem.

**OPEN QUESTION:** uma versão futura poderá adicionar padrões aninhados. As alternativas são
padrões estruturais completos, padrões limitados a um nível ou métodos auxiliares explícitos. A
recomendação PROPOSED é começar com padrões estruturais sem guards apenas depois que a análise de
exaustividade suportar diagnósticos igualmente claros.
