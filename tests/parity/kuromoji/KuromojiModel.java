//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import java.io.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.util.*;
import org.apache.lucene.analysis.ja.JapaneseAnalyzer;
import org.apache.lucene.analysis.ja.completion.KatakanaRomanizer;
import org.apache.lucene.util.CharsRef;
import org.apache.lucene.analysis.ja.dict.*;
import org.apache.lucene.analysis.morph.BinaryDictionary;
import org.apache.lucene.codecs.CodecUtil;
import org.apache.lucene.store.InputStreamDataInput;
import org.apache.lucene.util.IntsRef;
import org.apache.lucene.util.fst.*;

/** Exhaustive, versioned model export through Lucene's public APIs and codec readers. */
public class KuromojiModel {
  static final String[] CLASSES = {
    "NGRAM", "DEFAULT", "SPACE", "SYMBOL", "NUMERIC", "ALPHA", "CYRILLIC",
    "GREEK", "HIRAGANA", "KATAKANA", "KANJI", "KANJINUMERIC"
  };
  static final int CODE_POINT_COUNT = Character.MAX_CODE_POINT + 1;

  record TargetMap(int[] words, int[] offsets) {
    int size() { return offsets.length - 1; }
  }
  record Matrix(int forward, int backward) {}

  static void require(boolean condition, String message) throws IOException {
    if (!condition) throw new IOException(message);
  }

  static InputStream resource(String name) throws IOException {
    InputStream source = TokenInfoDictionary.class.getResourceAsStream(name);
    if (source == null) throw new FileNotFoundException("Missing Kuromoji resource: " + name);
    return new BufferedInputStream(source);
  }

  static InputStreamDataInput header(InputStream source, String codec) throws IOException {
    var input = new InputStreamDataInput(source);
    CodecUtil.checkHeader(input, codec, 1, 1);
    return input;
  }

  static TargetMap targetMap(String name) throws IOException {
    try (InputStream source = resource(name + "$targetMap.dat")) {
      var input = header(source, "kuromoji_dict_map");
      int[] words = new int[input.readVInt()];
      int[] offsets = new int[input.readVInt()];
      int wordId = 0;
      int sourceId = 0;
      for (int index = 0; index < words.length; index++) {
        int encoded = input.readVInt();
        if ((encoded & 1) != 0) {
          require(sourceId < offsets.length - 1, "Too many target-map sources");
          offsets[sourceId++] = index;
        }
        wordId = Math.addExact(wordId, encoded >>> 1);
        words[index] = wordId;
      }
      require(sourceId == offsets.length - 1 && offsets[0] == 0, "Invalid target-map source count");
      offsets[sourceId] = words.length;
      require(source.read() == -1, "Trailing target-map bytes");
      return new TargetMap(words, offsets);
    }
  }

  static Matrix matrixShape() throws IOException {
    try (InputStream source = resource("ConnectionCosts.dat")) {
      var input = header(source, "kuromoji_cc");
      int forward = input.readVInt();
      int backward = input.readVInt();
      require(forward > 0 && backward > 0, "Empty connection matrix");
      Math.multiplyExact(forward, backward);
      return new Matrix(forward, backward);
    }
  }

  static int[] wordIds(BinaryDictionary<?> dictionary, TargetMap mapping, int sourceId)
      throws IOException {
    require(sourceId >= 0 && sourceId < mapping.size(), "Surface outside target map");
    IntsRef actual = new IntsRef();
    dictionary.lookupWordIds(sourceId, actual);
    int start = mapping.offsets()[sourceId];
    int end = mapping.offsets()[sourceId + 1];
    require(actual.length == end - start, "Runtime target-map length differs");
    int[] result = Arrays.copyOfRange(mapping.words(), start, end);
    for (int index = 0; index < result.length; index++) {
      require(actual.ints[actual.offset + index] == result[index], "Runtime word order differs");
    }
    return result;
  }

  static void word(ModelStream output, JaMorphData model, int wordId, char[] surface,
      Matrix matrix) throws IOException {
    int left = model.getLeftId(wordId);
    int right = model.getRightId(wordId);
    int cost = model.getWordCost(wordId);
    require(left >= 0 && left < matrix.backward(), "Left context outside connection matrix");
    require(right >= 0 && right < matrix.forward(), "Right context outside connection matrix");
    require(cost >= Short.MIN_VALUE && cost <= Short.MAX_VALUE, "Word cost outside signed short");
    output.i32(wordId);
    output.i32(left);
    output.i32(right);
    output.i32(cost);
    output.text(model.getPartOfSpeech(wordId));
    output.text(model.getBaseForm(wordId, surface, 0, surface.length));
    output.text(model.getReading(wordId, surface, 0, surface.length));
    output.text(model.getPronunciation(wordId, surface, 0, surface.length));
    output.text(model.getInflectionType(wordId));
    output.text(model.getInflectionForm(wordId));
  }

  static void checkSurface(TokenInfoFST runtime, char[] surface, long expected) throws IOException {
    var arc = runtime.getFirstArc(new FST.Arc<Long>());
    var reader = runtime.getBytesReader();
    long output = 0;
    for (int index = 0; index < surface.length; index++) {
      require(runtime.findTargetArc(surface[index], arc, arc, index == 0, reader) != null,
          "Enumerated surface is absent from runtime FST");
      output = Math.addExact(output, arc.output());
    }
    require(arc.isFinal() && Math.addExact(output, arc.nextFinalOutput()) == expected,
        "Runtime surface output differs");
  }

  static void lexicon(Path directory, boolean verify, Matrix matrix, TargetMap mapping)
      throws IOException {
    TokenInfoDictionary dictionary = TokenInfoDictionary.getInstance();
    JaMorphData morphology = dictionary.getMorphAttributes();
    BitSet seen = new BitSet(mapping.size());
    int entryCount = 0;
    try (ModelStream output = new ModelStream(directory, "lexicon.bin", "UQAJLEX1", verify);
        InputStream source = resource("TokenInfoDictionary$fst.dat")) {
      var input = new InputStreamDataInput(source);
      var metadata = FST.readMetadata(input, PositiveIntOutputs.getSingleton());
      FST<Long> fst = new FST<>(metadata, input);
      require(source.read() == -1, "Trailing surface-FST bytes");
      var iterator = new IntsRefFSTEnum<>(fst);
      output.i32(mapping.size());
      output.i32(mapping.words().length);
      for (var item = iterator.next(); item != null; item = iterator.next()) {
        int sourceId = Math.toIntExact(item.output);
        require(sourceId >= 0 && sourceId < mapping.size() && !seen.get(sourceId),
            "Duplicate or invalid FST source ID");
        seen.set(sourceId);
        char[] surface = new char[item.input.length];
        for (int index = 0; index < surface.length; index++) {
          int label = item.input.ints[item.input.offset + index];
          require(label >= 0 && label <= Character.MAX_VALUE, "FST label outside UTF-16");
          surface[index] = (char) label;
        }
        checkSurface(dictionary.getFST(), surface, item.output);
        output.i32(sourceId);
        output.text(new String(surface));
        int[] ids = wordIds(dictionary, mapping, sourceId);
        output.i32(ids.length);
        entryCount = Math.addExact(entryCount, ids.length);
        for (int id : ids) word(output, morphology, id, surface, matrix);
      }
      require(seen.cardinality() == mapping.size(), "Not every target-map source is in the FST");
      require(entryCount == mapping.words().length, "Incomplete system word export");
    }
  }

  static void unknown(Path directory, boolean verify, Matrix matrix, TargetMap mapping)
      throws IOException {
    UnknownDictionary dictionary = UnknownDictionary.getInstance();
    JaMorphData morphology = dictionary.getMorphAttributes();
    require(mapping.size() == CLASSES.length, "Unknown dictionary class count differs");
    try (ModelStream output = new ModelStream(directory, "unknown.bin", "UQAJUNK1", verify)) {
      output.i32(mapping.size());
      output.i32(mapping.words().length);
      for (int classId = 0; classId < CLASSES.length; classId++) {
        require(CharacterDefinition.lookupCharacterClass(CLASSES[classId]) == classId,
            "Character class vocabulary differs");
        output.i32(classId);
        output.text(CLASSES[classId]);
        int[] ids = wordIds(dictionary, mapping, classId);
        output.i32(ids.length);
        for (int id : ids) word(output, morphology, id, new char[0], matrix);
      }
    }
  }

  static void costs(Path directory, boolean verify, Matrix matrix) throws IOException {
    ConnectionCosts runtime = ConnectionCosts.getInstance();
    try (ModelStream output = new ModelStream(directory, "connection_costs.bin", "UQAJCCS1", verify);
        InputStream source = resource("ConnectionCosts.dat")) {
      var input = header(source, "kuromoji_cc");
      require(input.readVInt() == matrix.forward() && input.readVInt() == matrix.backward(),
          "Connection matrix dimensions changed");
      output.i32(matrix.forward());
      output.i32(matrix.backward());
      int accumulated = 0;
      for (int backward = 0; backward < matrix.backward(); backward++) {
        for (int forward = 0; forward < matrix.forward(); forward++) {
          accumulated = Math.addExact(accumulated, input.readZInt());
          int expected = (short) accumulated;
          require(runtime.get(forward, backward) == expected, "Runtime connection cost differs");
          output.u16(expected & 0xffff);
        }
      }
      require(source.read() == -1, "Trailing connection-matrix bytes");
    }
  }

  static void characters(Path directory, boolean verify) throws IOException {
    CharacterDefinition runtime = CharacterDefinition.getInstance();
    require(CharacterDefinition.CLASS_COUNT == CLASSES.length, "Character class count differs");
    try (ModelStream output = new ModelStream(directory, "characters.bin", "UQAJCHR1", verify);
        InputStream source = resource("CharacterDefinition.dat")) {
      var input = header(source, "kuromoji_cd");
      byte[] categories = new byte[0x10000];
      input.readBytes(categories, 0, categories.length);
      byte[] flags = new byte[CLASSES.length];
      input.readBytes(flags, 0, flags.length);
      require(source.read() == -1, "Trailing character definition bytes");
      output.i32(CLASSES.length);
      for (byte flag : flags) {
        require((flag & ~3) == 0, "Unknown character flags");
        output.u8(flag);
      }
      output.i32(categories.length);
      for (int value = 0; value < categories.length; value++) {
        char character = (char) value;
        int category = Byte.toUnsignedInt(categories[value]);
        require(category < CLASSES.length, "Character class is out of range");
        require(runtime.getCharacterClass(character) == category, "Runtime character class differs");
        require(runtime.isInvoke(character) == ((flags[category] & 1) != 0), "Invoke flag differs");
        require(runtime.isGroup(character) == ((flags[category] & 2) != 0), "Group flag differs");
        output.u8(category);
        output.u8(runtime.isKanji(character) ? 1 : 0);
      }
    }
  }

  static void unicode(Path directory, boolean verify) throws IOException {
    require(Character.UnicodeScript.values().length <= 0x10000, "Too many Unicode scripts");
    try (ModelStream output = new ModelStream(directory, "unicode.bin", "UQAJUNI1", verify)) {
      output.i32(CODE_POINT_COUNT);
      for (int codePoint = 0; codePoint < CODE_POINT_COUNT; codePoint++) {
        output.u8(Character.getType(codePoint));
        output.u16(Character.UnicodeScript.of(codePoint).ordinal());
        output.u8((Character.isDigit(codePoint) ? 1 : 0)
            | (Character.isWhitespace(codePoint) ? 2 : 0)
            | (Character.isSpaceChar(codePoint) ? 4 : 0));
        output.i32(Character.toLowerCase(codePoint));
      }
    }
  }

  static List<String> stopWords() {
    TreeSet<String> result = new TreeSet<>();
    for (Object word : JapaneseAnalyzer.getDefaultStopSet()) result.add(new String((char[]) word));
    return List.copyOf(result);
  }

  static Map<String, List<String>> completionMappings() throws IOException {
    var result = new TreeMap<String, List<String>>();
    try (InputStream source = KatakanaRomanizer.class.getResourceAsStream("romaji_map.txt")) {
      require(source != null, "Missing completion mapping");
      try (var reader = new BufferedReader(new InputStreamReader(source, StandardCharsets.UTF_8))) {
        for (String line = reader.readLine(); line != null; line = reader.readLine()) {
          if (line.startsWith("#")) continue;
          String[] columns = line.trim().split(",");
          if (columns.length < 2) continue;
          result.put(columns[0], List.of(Arrays.copyOfRange(columns, 1, columns.length)));
        }
      }
    }
    for (var entry : result.entrySet()) {
      List<String> actual = KatakanaRomanizer.getInstance().romanize(new CharsRef(entry.getKey()))
          .stream().map(CharsRef::toString).toList();
      require(actual.equals(entry.getValue()), "Runtime completion mapping or order differs");
    }
    return result;
  }

  static void analysis(Path directory, boolean verify, List<String> words,
      List<String> tags, Map<String, List<String>> mappings) throws IOException {
    try (ModelStream output = new ModelStream(directory, "analysis.bin", "UQAJANA1", verify)) {
      output.i32(words.size());
      for (String word : words) output.text(word);
      output.i32(tags.size());
      for (String tag : tags) output.text(tag);
      output.i32(mappings.size());
      for (var entry : mappings.entrySet()) {
        output.text(entry.getKey());
        output.i32(entry.getValue().size());
        for (String value : entry.getValue()) output.text(value);
      }
    }
  }

  static Map<String, Object> object(Object... pairs) {
    Map<String, Object> result = new LinkedHashMap<>();
    for (int index = 0; index < pairs.length; index += 2) {
      result.put((String) pairs[index], pairs[index + 1]);
    }
    return result;
  }

  static String json(Object value) {
    if (value instanceof String text) {
      StringBuilder result = new StringBuilder("\"");
      for (char character : text.toCharArray()) {
        if (character == '"' || character == '\\') result.append('\\').append(character);
        else if (character < 32 || Character.isSurrogate(character)) {
          result.append(String.format("\\u%04x", (int) character));
        } else result.append(character);
      }
      return result.append('"').toString();
    }
    if (value instanceof Map<?, ?> map) {
      List<String> fields = new ArrayList<>();
      map.forEach((key, item) -> fields.add(json(key) + ":" + json(item)));
      return "{" + String.join(",", fields) + "}";
    }
    if (value instanceof List<?> list) {
      return "[" + String.join(",", list.stream().map(KuromojiModel::json).toList()) + "]";
    }
    if (value instanceof Number || value instanceof Boolean) return value.toString();
    throw new IllegalArgumentException("Unsupported JSON value: " + value);
  }

  public static void main(String[] args) throws Exception {
    if (args.length != 2 || (!args[0].equals("export") && !args[0].equals("verify"))) {
      throw new IllegalArgumentException("usage: KuromojiModel.java export|verify directory");
    }
    Path directory = Path.of(args[1]);
    require(Files.isDirectory(directory), "Output directory does not exist");
    Matrix matrix = matrixShape();
    TargetMap system = targetMap("TokenInfoDictionary");
    TargetMap unknown = targetMap("UnknownDictionary");
    boolean verifyOnly = args[0].equals("verify");
    List<String> words = stopWords();
    List<String> tags = new TreeSet<>(JapaneseAnalyzer.getDefaultStopTags()).stream().toList();
    Map<String, List<String>> mappings = completionMappings();
    if (!verifyOnly) {
      lexicon(directory, false, matrix, system);
      unknown(directory, false, matrix, unknown);
      costs(directory, false, matrix);
      characters(directory, false);
      unicode(directory, false);
      analysis(directory, false, words, tags, mappings);
    }
    lexicon(directory, true, matrix, system);
    unknown(directory, true, matrix, unknown);
    costs(directory, true, matrix);
    characters(directory, true);
    unicode(directory, true);
    analysis(directory, true, words, tags, mappings);
    System.out.println(json(object(
        "runtime", object("java_version", System.getProperty("java.version"),
            "java_runtime_version", System.getProperty("java.runtime.version"),
            "java_vendor", System.getProperty("java.vendor")),
        "surface_count", system.size(), "word_count", system.words().length,
        "unknown_class_count", unknown.size(), "unknown_word_count", unknown.words().length,
        "matrix_forward", matrix.forward(), "matrix_backward", matrix.backward(),
        "unicode_count", CODE_POINT_COUNT, "character_count", 0x10000,
        "morphology_fields", List.of("part_of_speech", "base_form", "reading", "pronunciation",
            "inflection_type", "inflection_form"),
        "stop_word_count", words.size(), "stop_tag_count", tags.size(),
        "completion_mapping_count", mappings.size(), "character_classes", List.of(CLASSES),
        "unicode_scripts", Arrays.stream(Character.UnicodeScript.values()).map(Enum::name).toList())));
  }

  /** A big-endian neutral stream, checked field by field against the live model on read. */
  static class ModelStream implements AutoCloseable {
    final String name;
    final DataInputStream input;
    final DataOutputStream output;

    ModelStream(Path directory, String name, String magic, boolean verify) throws IOException {
      this.name = name;
      Path path = directory.resolve(name);
      input = verify ? new DataInputStream(new BufferedInputStream(Files.newInputStream(path))) : null;
      output = verify ? null : new DataOutputStream(new BufferedOutputStream(
          Files.newOutputStream(path, StandardOpenOption.CREATE_NEW, StandardOpenOption.WRITE)));
      for (byte value : magic.getBytes(StandardCharsets.US_ASCII)) u8(value);
    }

    void i32(int value) throws IOException {
      if (output != null) output.writeInt(value);
      else require(input.readInt() == value, name + ": 32-bit field differs");
    }

    void u16(int value) throws IOException {
      require(value >= 0 && value <= 0xffff, name + ": invalid 16-bit value");
      if (output != null) output.writeShort(value);
      else require(input.readUnsignedShort() == value, name + ": 16-bit field differs");
    }

    void u8(int value) throws IOException {
      require(value >= 0 && value <= 0xff, name + ": invalid 8-bit value");
      if (output != null) output.writeByte(value);
      else require(input.readUnsignedByte() == value, name + ": 8-bit field differs");
    }

    void text(String value) throws IOException {
      i32(value == null ? -1 : value.length());
      if (value != null) {
        for (int index = 0; index < value.length(); index++) u16(value.charAt(index));
      }
    }

    public void close() throws IOException {
      if (output != null) output.close();
      if (input != null) {
        try { require(input.read() == -1, name + ": trailing bytes"); }
        finally { input.close(); }
      }
    }
  }
}
