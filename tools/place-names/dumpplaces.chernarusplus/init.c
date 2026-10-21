// Writes every world's CfgWorlds >> Names list to $profile:placenames.json, then quits.
// The game decrypts world configs as it loads them, so this also covers the encrypted ones.
string Esc(string s)
{
	s.Replace("\\", "\\\\");
	s.Replace("\"", "\\\"");
	return s;
}

void DumpPlaces()
{
	FileHandle f = OpenFile("$profile:placenames.json", FileMode.WRITE);
	FPrintln(f, "{");
	int worlds = GetGame().ConfigGetChildrenCount("CfgWorlds");
	bool firstWorld = true;
	for (int w = 0; w < worlds; w++)
	{
		string world;
		GetGame().ConfigGetChildName("CfgWorlds", w, world);
		string names = "CfgWorlds " + world + " Names";
		if (!GetGame().ConfigIsExisting(names))
			continue;
		if (!firstWorld)
			FPrintln(f, ",");
		firstWorld = false;
		FPrintln(f, "\"" + Esc(world) + "\": [");
		int count = GetGame().ConfigGetChildrenCount(names);
		for (int i = 0; i < count; i++)
		{
			string cls;
			GetGame().ConfigGetChildName(names, i, cls);
			string p = names + " " + cls;
			string name;
			string type;
			GetGame().ConfigGetTextRaw(p + " name", name);
			GetGame().ConfigGetText(p + " type", type);
			TFloatArray pos = new TFloatArray;
			GetGame().ConfigGetFloatArray(p + " position", pos);
			if (pos.Count() < 2)
				continue;
			string sep = ",";
			if (i == count - 1)
				sep = "";
			FPrintln(f, "{\"name\": \"" + Esc(name) + "\", \"type\": \"" + Esc(type) + "\", \"x\": " + pos[0].ToString() + ", \"z\": " + pos[1].ToString() + "}" + sep);
		}
		FPrint(f, "]");
	}
	FPrintln(f, "");
	FPrintln(f, "}");
	CloseFile(f);
	Print("place names written");
}

void main()
{
	DumpPlaces();
	GetGame().RequestExit(0);
}

Mission CreateCustomMission(string path)
{
	return new MissionServer();
}
